# rs-f4ss 扩展规格 (v2)

**Version**: 2.1.0
**Status**: Phase 2 Complete (M5-M11 已实现)
**Date**: 2026-06-05
**基于**: Phase 1 + Phase 2 已完成 (164 单元测试 + 51 E2E + 43 E2E API)

> **Note**: M5 (Feature Flags), M6 (HTTP Backend), M7 (REST API), M8 (Web UI), M9 (Tauri Desktop),
> M10 (Daemon Mode), M11 (Performance Optimization) are all complete. M9-M10 were not in the
> original v2 spec but were added during implementation. S3 Backend (M12) and WebDAV Server
> are deferred to Phase 3.

---

## 1. 目标

将 rs-f4ss 从"单一 WebDAV 挂载工具"演进为"多协议文件系统桥接平台"：

1. **多协议后端**：WebDAV → S3/MinIO → 百度网盘 → 可扩展
2. **REST API 管理**：动态添加/移除挂载点，无需重启进程
3. **WebDAV Server**：聚合多种后端，对外暴露统一 WebDAV 接口
4. **Feature Flag**：按需编译，最小二进制只包含 WebDAV 挂载

### 成功标准

- [ ] `cargo build --no-default-features --features webdav` 生成与当前大小相当的二进制
- [ ] `cargo build --features webdav,s3` 额外增加 < 200KB
- [ ] `cargo test --all` 97+ 测试通过（现有测试零回归）
- [ ] REST API 可在一个进程内管理多个挂载点
- [ ] WebDAV Server 可聚合 S3 + WebDAV 后端，客户端透明访问
- [ ] 每个新功能都有对应的单元测试 + 集成测试

---

## 2. 设计原则

### 2.1 渐进式扩展

```
Phase 1 (已完成)     Phase 2 (本规格)       Phase 3 (未来)
─────────────     ──────────────────     ──────────────
WebDAV backend    + Feature flags        + 百度网盘 backend
FUSE mount        + S3/MinIO backend     + FTP backend
CLI               + REST API 管理        + Tauri 桌面 UI
                  + WebDAV Server        + Web 管理界面
```

每个 Phase 独立可用，不依赖后续 Phase。

### 2.2 保护现有成果

- 现有 97 单元测试 + E2E 必须始终通过
- `rs-f4ss http://host:5000 /mnt/test -f` 命令行用法不变
- 现有代码路径性能不退化
- Feature flag `default = ["webdav"]` 保持向后兼容

### 2.3 最小依赖原则

- 新后端只引入必要的依赖（S3 需要 `hmac`+`sha2`+`hex`，不引入完整 AWS SDK）
- axum 只在 `api` 或 `server` feature 启用时编译
- 平台挂载代码（fuser/winfsp）与协议代码正交

---

## 3. 架构设计

### 3.1 总体分层

```
┌─────────────────────────────────────────────────────────────────┐
│                      Frontend Adapters                          │
│  ┌──────────┐  ┌────────────────────────────────────────────┐  │
│  │ CLI      │  │ REST API (axum) — 动态挂载管理              │  │
│  │ (clap)   │  │ POST/GET/DELETE /mounts                    │  │
│  └────┬─────┘  └─────────────┬──────────────────────────────┘  │
│       │                      │                                  │
│  ┌────┴──────────────────────┴──────────────────────────────┐  │
│  │                  MountManager (新增)                       │  │
│  │  mounts: HashMap<MountId, MountHandle>                   │  │
│  │  - add(config) → spawn mount thread → MountHandle       │  │
│  │  - remove(id)  → signal unmount → join thread           │  │
│  │  - list()      → Vec<MountInfo>                         │  │
│  └──────────────────────────┬───────────────────────────────┘  │
│                             │                                    │
│  ┌──────────────────────────┴───────────────────────────────┐  │
│  │              rs-f4ss-core (现有)                        │  │
│  │  FuseAdapter │ CacheLayer │ HandleTable │ InodeMap        │  │
│  │  MountEngine  │ MountEvent │ MountStatus                   │  │
│  ├──────────────────────────────────────────────────────────┤  │
│  │              StorageBackend trait (现有)                   │  │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌────────────┐  │  │
│  │  │ WebDAV   │ │ S3/MinIO │ │ 百度网盘  │ │ WebDAV Srv │  │  │
│  │  │ (现有)    │ │ (新增)    │ │ (未来)    │ │ 聚合器(新) │  │  │
│  │  └──────────┘ └──────────┘ └──────────┘ └────────────┘  │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### 3.2 关键设计：StorageBackend trait 不变

现有 trait 完全满足扩展需求，**不做任何修改**：

```rust
// 保持不变 — 8 个 async 方法 + 2 个同步方法
#[async_trait]
pub trait StorageBackend: Send + Sync + 'static {
    fn protocol(&self) -> &str;
    fn server_addr(&self) -> &str;
    fn is_read_only(&self) -> bool { false }
    async fn list(&self, path: &str) -> Result<Vec<Entry>, BackendError>;
    async fn stat(&self, path: &str) -> Result<Entry, BackendError>;
    async fn read(&self, path: &str, offset: u64, size: u32) -> Result<Vec<u8>, BackendError>;
    async fn write(&self, path: &str, data: &[u8]) -> Result<(), BackendError>;
    async fn mkdir(&self, path: &str) -> Result<(), BackendError>;
    async fn delete(&self, path: &str) -> Result<(), BackendError>;
    async fn rename(&self, from: &str, to: &str) -> Result<(), BackendError>;
    async fn ping(&self) -> Result<bool, BackendError> { Ok(true) }
}
```

### 3.3 关键设计：MountManager (新增)

```rust
// crates/rs-f4ss-core/src/manager.rs (新文件)
pub struct MountId(String);  // UUID

pub struct MountInfo {
    pub id: MountId,
    pub protocol: String,
    pub server_addr: String,
    pub mountpoint: PathBuf,
    pub status: MountStatus,
    pub read_only: bool,
}

pub struct MountHandle {
    pub info: MountInfo,
    cancel: Arc<AtomicBool>,        // 通知 mount 线程停止
    thread: Option<JoinHandle<()>>, // mount 线程句柄
}

pub struct MountManager {
    mounts: DashMap<MountId, MountHandle>,
}

impl MountManager {
    pub fn new() -> Self;
    pub async fn add(&self, config: MountConfig, backend: Box<dyn StorageBackend>)
        -> Result<MountId, MountError>;
    pub async fn remove(&self, id: &MountId) -> Result<MountInfo, MountError>;
    pub fn list(&self) -> Vec<MountInfo>;
    pub fn get(&self, id: &MountId) -> Option<MountInfo>;
}
```

### 3.4 关键设计：WebDAV Server (聚合器)

```rust
// crates/rs-f4ss-core/src/server.rs (新文件，feature = "server")
//
// 将 StorageBackend trait 的方法映射到 WebDAV HTTP 方法：
//
// HTTP Method    →  StorageBackend 方法
// ────────────────────────────────────────
// PROPFIND depth=0  →  stat()
// PROPFIND depth=1  →  list() + stat()
// GET / Range       →  read()
// PUT               →  write()
// MKCOL             →  mkdir()
// DELETE            →  delete()
// MOVE              →  rename()
//
// 路由映射：
// 每个后端分配一个 URL 前缀，如 /s3/ → S3Backend，/webdav/ → WebDavBackend
// 请求 /s3/path/to/file → backend.read("/path/to/file")
```

### 3.5 协议映射矩阵

| 操作 | WebDAV | S3 | MinIO | 百度网盘 |
|------|--------|----|----|---------|
| list | PROPFIND depth=1 | ListObjectsV2 | (=S3) | /file/list |
| stat | PROPFIND depth=0 | HeadObject | (=S3) | /file/meta |
| read | GET + Range | GetObject Range | (=S3) | /file/download |
| write | PUT | PutObject | (=S3) | /file/upload |
| mkdir | MKCOL | PutObject key后缀/ | (=S3) | /file/mkdir |
| delete | DELETE | DeleteObject | (=S3) | /file/delete |
| rename | MOVE Destination | CopyObject+Delete | (=S3) | /file/move |
| ping | PROPFIND / | HeadBucket | (=S3) | /user/info |

---

## 4. Feature Flag 设计

### 4.1 Cargo.toml 配置

```toml
# crates/rs-f4ss-core/Cargo.toml
[features]
default = ["webdav"]

# ── 后端协议 ──
webdav = ["reqwest", "quick-xml", "base64", "chrono"]
s3     = ["reqwest", "quick-xml", "chrono", "hmac", "sha2", "hex"]

# ── 挂载能力 ──
fuse-mount    = ["fuser"]            # Linux FUSE (cfg + feature 双重门)
winfsp-mount  = ["winfsp"]           # Windows WinFsp

# ── 服务能力 ──
api    = ["axum", "tower-http", "serde_json"]  # REST API 管理
server = ["axum", "tower-http", "quick-xml"]    # WebDAV Server
```

### 4.2 cfg 门控模式

```rust
// lib.rs
#[cfg(feature = "webdav")]
pub mod backend::webdav;

#[cfg(feature = "s3")]
pub mod backend::s3;

#[cfg(feature = "api")]
pub mod manager;

#[cfg(feature = "server")]
pub mod server;
```

### 4.3 编译组合

```bash
# 最小：仅 WebDAV 挂载（与当前行为相同）
cargo build --no-default-features --features webdav,fuse-mount

# REST API + WebDAV + S3
cargo build --features webdav,s3,api

# WebDAV Server（聚合模式，无本地挂载）
cargo build --no-default-features --features webdav,s3,server

# 全功能
cargo build --all-features
```

### 4.4 依赖隔离

| Feature | 额外依赖 | 增量大小估算 |
|---------|---------|------------|
| webdav (默认) | reqwest, quick-xml, base64, chrono | 基线 ~4MB |
| s3 | +hmac, sha2, hex | +~200KB |
| api | +axum, tower-http, serde_json | +~800KB |
| server | +axum, tower-http | +~600KB |

---

## 5. 模块结构（扩展后）

```
crates/rs-f4ss-core/src/
├── lib.rs                 # cfg-gated module declarations (扩展)
├── error.rs               # BackendError + MountError (不变)
├── cache.rs               # CacheLayer (不变)
├── handle.rs              # HandleTable (不变)
├── inode.rs               # InodeMap (不变)
├── mount.rs               # FuseAdapter + MountEngine (不变)
├── mount_linux.rs         # fuser impl (不变)
├── mount_windows.rs       # WinFsp impl (不变)
├── manager.rs             # MountManager (新增, feature = "api")
├── server.rs              # WebDAV Server 路由 (新增, feature = "server")
├── backend/
│   ├── mod.rs             # StorageBackend trait + Box delegation (不变)
│   ├── types.rs           # Entry (不变)
│   ├── webdav.rs          # WebDAV backend (不变, feature = "webdav")
│   └── s3.rs              # S3/MinIO backend (新增, feature = "s3")

crates/rs-f4ss-cli/src/
├── main.rs                # CLI + resolve_backend (扩展: s3:// URL scheme)
├── os_linux.rs            # Linux helpers (不变)
├── os_windows.rs          # Windows helpers (不变)
```

---

## 6. REST API 设计

### 6.1 端点

| Method | Path | 说明 |
|--------|------|------|
| POST | /mounts | 创建挂载 |
| GET | /mounts | 列出所有挂载 |
| GET | /mounts/{id} | 查询单个挂载状态 |
| DELETE | /mounts/{id} | 卸载并移除 |

### 6.2 请求/响应格式

```json
// POST /mounts
{
  "url": "s3://bucket-name",
  "mountpoint": "/mnt/s3",
  "read_only": false,
  "cache_ttl": 5,
  "s3": {
    "endpoint": "https://s3.amazonaws.com",
    "region": "us-east-1",
    "access_key": "...",
    "secret_key": "..."
  }
}

// Response 201
{
  "id": "a1b2c3d4",
  "protocol": "s3",
  "server_addr": "bucket-name",
  "mountpoint": "/mnt/s3",
  "status": "mounted",
  "read_only": false
}
```

### 6.3 运行模式

```bash
# 模式 1: CLI 直接挂载（与现在相同，不变）
rs-f4ss http://host:5000 /mnt/test -f

# 模式 2: API 服务模式
rs-f4ss serve --listen 0.0.0.0:8080
# 然后通过 HTTP 管理挂载

# 模式 3: API 服务 + 初始挂载
rs-f4ss serve --listen 0.0.0.0:8080 \
  --mount http://host:5000,/mnt/test \
  --mount s3://bucket,/mnt/s3,access_key=...,secret_key=...
```

---

## 7. WebDAV Server 设计

### 7.1 URL 路由

```
http://localhost:8081/
├── /s3/           → S3Backend 的文件
│   ├── bucket-a/
│   └── bucket-b/
├── /webdav/       → WebDavBackend 的文件
│   └── remote/
└── /local/        → 本地文件系统后端（未来）
```

### 7.2 启动方式

```bash
# WebDAV Server 模式（聚合多个后端）
rs-f4ss share serve --listen 0.0.0.0:8081 \
  --backend s3://bucket,/s3/ \
  --backend http://remote:5000,/webdav/
```

---

## 8. S3 Backend 设计

### 8.1 构造函数

```rust
// crates/rs-f4ss-core/src/backend/s3.rs
pub struct S3Backend {
    endpoint: String,     // https://s3.amazonaws.com
    region: String,       // us-east-1
    bucket: String,       // my-bucket
    access_key: String,
    secret_key: String,
    client: reqwest::Client,
}

impl S3Backend {
    pub fn new(
        endpoint: &str,
        region: &str,
        bucket: &str,
        access_key: &str,
        secret_key: &str,
    ) -> Result<Self, BackendError>;
}
```

### 8.2 AWS Signature V4 签名

使用 `hmac-sha256` 手动实现 V4 签名，不引入完整 AWS SDK：

```
签名字符串 = HTTP Method + URL Path + Query + Headers + Payload Hash
签名密钥 = HMAC-SHA256 派生链 (date/region/service/request)
Authorization: AWS4-HMAC-SHA256 Credential=... SignedHeaders=... Signature=...
```

### 8.3 语义映射

```rust
impl StorageBackend for S3Backend {
    async fn list(&self, path: &str) -> Result<Vec<Entry>, BackendError> {
        // GET /?list-type=2&prefix={path}&delimiter=/
    }

    async fn mkdir(&self, path: &str) -> Result<(), BackendError> {
        // S3 没有 true directory，用 zero-byte object 以 "/" 结尾
        // PUT /{path}/ (empty body, content-length=0)
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), BackendError> {
        // CopyObject(to) + DeleteObject(from)，非原子
    }
}
```

---

## 9. 实施计划

### Phase 2A: Feature Flag 基础设施 (1-2 天)

重构现有代码，为 feature flag 做准备。

- [ ] T2A-1: 重构 `Cargo.toml` 添加 feature 定义
- [ ] T2A-2: 用 `#[cfg(feature = "webdav")]` 门控 webdav 模块
- [ ] T2A-3: 验证 `--no-default-features --features webdav` 编译通过
- [ ] T2A-4: 运行全部测试，确保零回归
- [ ] T2A-5: 更新 `CLAUDE.md` 构建命令

**验证**: `cargo test --all` 97+ pass, `cargo build --no-default-features --features webdav` 成功

### Phase 2B: S3 Backend (2-3 天)

实现 S3/MinIO 后端，TDD 驱动。

- [ ] T2B-1: 编写 S3 签名单元测试 (HMAC 计算)
- [ ] T2B-2: 实现 AWS V4 签名函数
- [ ] T2B-3: 编写 S3Backend 构造函数测试
- [ ] T2B-4: 实现 `list()` + XML 解析 (ListObjectsV2)
- [ ] T2B-5: 实现 `stat()` (HeadObject)
- [ ] T2B-6: 实现 `read()` (GetObject + Range)
- [ ] T2B-7: 实现 `write()` (PutObject)
- [ ] T2B-8: 实现 `mkdir()` / `delete()` / `rename()`
- [ ] T2B-9: CLI 集成：`s3://bucket` URL scheme 路由
- [ ] T2B-10: 对接 MinIO 兼容端点测试

**验证**: 单元测试覆盖所有 S3 操作，`rs-f4ss s3://bucket /mnt/s3` 可用

### Phase 2C: MountManager + REST API (2-3 天)

动态挂载管理。

- [ ] T2C-1: 编写 MountManager 单元测试
- [ ] T2C-2: 实现 MountManager (add/remove/list)
- [ ] T2C-3: 编写 REST API 路由测试 (axum::test)
- [ ] T2C-4: 实现 POST /mounts 端点
- [ ] T2C-5: 实现 GET /mounts, GET /mounts/{id} 端点
- [ ] T2C-6: 实现 DELETE /mounts/{id} 端点
- [ ] T2C-7: CLI 新增 `serve` 子命令

**验证**: `curl -X POST /mounts` 创建挂载，`curl GET /mounts` 列出挂载

### Phase 2D: WebDAV Server (2 天)

聚合多后端暴露 WebDAV 接口。

- [ ] T2D-1: 编写 WebDAV Server 路由测试
- [ ] T2D-2: 实现 PROPFIND → stat/list 映射
- [ ] T2D-3: 实现 GET/PUT/MKCOL/DELETE/MOVE 映射
- [ ] T2D-4: 实现多后端路由分发 (URL 前缀 → Backend)
- [ ] T2D-5: CLI 新增 `serve-server` 子命令

**验证**: `curl -X PROPFIND http://localhost:8081/s3/` 返回 XML 目录列表

### Phase 2E: 文档同步 (1 天)

- [ ] T2E-1: 更新 `README.md` (新增协议、API 用法、编译选项)
- [ ] T2E-2: 更新 `CLAUDE.md` (架构、构建命令、测试命令)
- [ ] T2E-3: 更新 `docs/SPEC.md` (合并 v2 内容)
- [ ] T2E-4: 更新 `docs/ADR.md` (新增架构决策记录)
- [ ] T2E-5: 更新 `docs/TEST_PLAN.md` (新增测试策略)
- [ ] T2E-6: 更新 `docs/TASKS.md` (任务完成状态)

---

## 10. 测试策略

### 10.1 测试分层

| 层级 | 工具 | 覆盖内容 | 目标数量 |
|------|------|---------|---------|
| 单元 | `#[test]` | 签名算法、XML解析、MountManager逻辑 | 20-30 新增 |
| 集成 | `#[test]` + MockBackend | REST API路由、WebDAV Server路由 | 10-15 新增 |
| E2E | bash/ps1 + real S3 | 完整挂载→操作→卸载 | 10-15 新增 |

### 10.2 TDD 规则（延续 Phase 1）

每个任务遵循 RED → GREEN → REFACTOR：

```
1. 写失败测试
2. 写最小实现让测试通过
3. 重构，保持测试通过
4. cargo test --all 确认零回归
```

### 10.3 Feature Flag 测试

每个 feature 组合至少验证编译：

```bash
cargo test --no-default-features --features webdav
cargo test --features webdav,s3
cargo test --features webdav,s3,api
cargo test --all-features
```

---

## 11. 边界与约束

### 始终遵守
- 每次提交前 `cargo test --all` + `cargo clippy` 通过
- Feature flag 不得引入循环依赖
- 新代码必须有对应测试
- 文档与代码同步更新

### 先确认再执行
- 引入新的 workspace 依赖
- 修改 `StorageBackend` trait 签名
- 修改 `MountEngine` 公共 API
- 改变 CLI 子命令结构

### 绝不执行
- 删除现有通过的测试
- 修改 `StorageBackend` trait 的 8 个方法签名（只允许新增带默认实现的方法）
- 在非 feature-gated 代码中引入 feature-gated 依赖
- 提交包含密钥/凭证的代码

---

## 12. 风险与缓解

| 风险 | 影响 | 缓解策略 |
|------|------|---------|
| S3 V4 签名实现有 bug | 认证失败 | 先用 MinIO 本地测试，再测 AWS |
| FUSE mount 线程与 axum runtime 冲突 | 运行时 panic | mount 用 std::thread，axum 用 tokio，各自独立 |
| S3 rename 非原子 | 并发竞态 | 文档标注限制，返回 NotSupported 作为选项 |
| Feature flag 组合爆炸 | CI 复杂度 | 只测试关键组合 (4-5 种) |
| 二进制体积增长 | 部署困难 | 每个 feature 独立控制，default 最小 |

---

## 13. 开放问题

1. **S3 大文件分片上传**：`write()` 当前接收完整 `&[u8]`，S3 multipart upload 是否需要 trait 扩展？建议：Phase 2 不做，Phase 3 考虑在 trait 加 `write_chunk()` 带默认实现。

2. **WebDAV Server 认证**：聚合服务器是否需要自己的认证层？建议：Phase 2 先不做认证，通过绑定 localhost 使用。

3. **MountManager 持久化**：挂载配置是否持久化到磁盘，重启后恢复？建议：Phase 2 不做，Phase 3 加 JSON 配置文件。

4. **API Server TLS**：REST API 是否支持 HTTPS？建议：Phase 2 不做，生产环境用反向代理。
