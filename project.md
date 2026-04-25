# NFSServe 项目文档

## 一、项目要求

### 1.1 核心目标
实现一个**单 binary、单端口**的 NFSv3 服务器，基于真实文件系统（非内存模拟），支持完整的文件操作。

### 1.2 功能要求
- **单 binary**：`cargo build` 产出单个可执行文件
- **单端口**：NFS + Mount + Portmap 共用一个 TCP 端口（不需要系统 portmap 111 端口）
- **真实文件系统**：所有操作直接映射到本地目录（read/write/create/mkdir/remove/rename/symlink 等）
- **完整操作支持**：实现 `NFSFileSystem` trait 的全部方法
- **鉴权机制**：支持 IP 白名单 + UID 白名单
- **HTTP 文件访问**：增加 WebDAV 协议支持，可被操作系统原生挂载

### 1.3 运行环境
- Linux（主要目标平台）
- 基于 tokio 异步运行时
- 支持内网文件共享场景

---

## 二、整体设计

### 2.1 架构图

```
┌─────────────────────────────────────────────────────────────┐
│                        nfsserve binary                        │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────────┐  │
│  │   NFSv3     │    │   Mount     │    │   Portmap       │  │
│  │  Handler    │    │  Handler    │    │   Handler       │  │
│  └──────┬──────┘    └──────┬──────┘    └────────┬────────┘  │
│         │                  │                     │           │
│         └──────────────────┼─────────────────────┘           │
│                            │                                  │
│                    ┌───────┴───────┐                          │
│                    │  RPC Router   │  ← rpcwire.rs            │
│                    │  (单端口多路)  │                          │
│                    └───────┬───────┘                          │
│                            │                                  │
│         ┌──────────────────┼──────────────────┐              │
│         │                  │                  │              │
│  ┌──────┴──────┐   ┌──────┴──────┐   ┌──────┴──────┐       │
│  │  TCP Listener│   │  HTTP Server │   │  Transaction │       │
│  │  (port 2049) │   │  (WebDAV)    │   │  Tracker     │       │
│  └──────────────┘   └──────────────┘   └──────────────┘       │
│                            │                                  │
│                    ┌───────┴───────┐                          │
│                    │   MirrorFS    │  ← 真实文件系统后端      │
│                    │  (inode缓存)  │                          │
│                    └───────┬───────┘                          │
│                            │                                  │
│                    ┌───────┴───────┐                          │
│                    │  本地文件系统  │                          │
│                    └───────────────┘                          │
└─────────────────────────────────────────────────────────────┘
```

### 2.2 核心模块

| 模块 | 文件 | 职责 |
|------|------|------|
| **RPC 消息路由** | `rpcwire.rs` | 在单 TCP 端口上根据 Program Number 分发到 NFS/Mount/Portmap |
| **NFS 协议** | `nfs.rs` + `nfs_handlers.rs` | NFSv3 XDR 结构定义 + 22 个 RPC 方法处理 |
| **Mount 协议** | `mount.rs` + `mount_handlers.rs` | MNT/EXPORT/UMNT 处理 |
| **Portmap** | `portmap.rs` + `portmap_handlers.rs` | PMAPPROC_GETPORT（伪实现，始终返回当前端口）|
| **文件系统后端** | `mirrorfs.rs` | 基于本地目录的完整 NFSFileSystem 实现 |
| **WebDAV 适配器** | `webdav.rs` | 将 MirrorFS 适配为 dav-server 的 DavFileSystem trait |
| **鉴权** | `rpcwire.rs` + `context.rs` | IP 白名单 + UID 白名单检查 |
| **工具函数** | `fs_util.rs` | 元数据转换（metadata ↔ fattr3）、属性设置 |

### 2.3 关键技术决策

#### 2.3.1 单端口设计
NFS 标准需要三个端口（portmap 111, mount, nfs），但现代客户端支持 `-o port=xxx,mountport=xxx` 显式指定。本项目将三个协议复用到一个端口，简化部署。

#### 2.3.2 MirrorFS 缓存策略
- 使用 `intaglio` 字符串驻留优化路径比较
- 维护 `fileid → 路径` 和 `路径 → fileid` 双向映射
- 按 mtime 自动刷新缓存，检测底层文件系统变更
- 目录列表按需填充子节点

#### 2.3.3 鉴权设计
- **IP 白名单**：在 RPC 层统一拦截，非白名单 IP 返回 `AUTH_ERROR`
- **UID 白名单**：解析 `AUTH_UNIX` 的 uid，非授权用户拒绝
- 默认无限制（内网可信场景）

#### 2.3.4 WebDAV 集成
- 基于 `dav-server` crate（通过 Litmus 测试套件验证 RFC 4918 兼容性）
- 自定义 `DavFileSystem` 适配器复用 `MirrorFS`
- 使用 `FakeLs` 提供最小锁支持（满足 macOS/Windows 客户端需求）

---

## 三、当前进度

### 3.1 已完成 ✅

| 功能 | 状态 | 说明 |
|------|------|------|
| NFSv3 服务端 | ✅ | 完整协议栈，支持所有核心操作 |
| 单 binary | ✅ | `cargo build --release` 产出 `nfsserve` |
| 单端口 | ✅ | Portmap + Mount + NFS 共用一个端口 |
| 真实文件系统 | ✅ | `MirrorFS` 完整实现，直接操作本地目录 |
| 文件读写 | ✅ | read/write/create/remove/rename/mkdir/symlink |
| 鉴权（IP） | ✅ | `--allow-ip` 白名单 |
| 鉴权（UID） | ✅ | `--allow-uid` 白名单 |
| WebDAV | ✅ | 可挂载的 HTTP 文件系统，`--dav-port` 启动 |
| Fork 管理脚本 | ✅ | `fork.sh` 支持同步上游、分支管理 |

### 3.2 命令行接口

```bash
# NFS 模式
./nfsserve /data 0.0.0.0:2049 --allow-ip 192.168.1.41 --allow-uid 0,1000

# WebDAV 模式
./nfsserve /data --dav-port 8080

# 双协议
./nfsserve /data 0.0.0.0:2049 --dav-port 8080
```

### 3.3 客户端支持

| 协议 | Linux | macOS | Windows |
|------|-------|-------|---------|
| NFS | `mount -t nfs -o nolock,vers=3,tcp,port=2049,mountport=2049` | `mount_nfs` | 原生 NFS Client |
| WebDAV | `mount -t davfs http://host:8080/` | Finder 连接服务器 | 映射网络驱动器 |

### 3.4 已知限制 ⚠️

| 限制 | 说明 |
|------|------|
| 并发写冲突 | NFSv3 无状态设计，多客户端同时写同一文件会产生覆盖（协议本身限制）|
| Windows NFS | Windows 客户端会尝试旧版 NFS 协议，会产生一些未实现 API 的日志 |
| WebDAV 性能 | HTTP/XML 开销大于 NFS，大文件/高频操作场景性能较低 |
| 锁机制 | WebDAV 使用 FakeLs（假锁），不支持真正的跨客户端文件锁 |

### 3.5 下一步（可选）
- [ ] 实现 NLM（Network Lock Manager）提供真正的文件锁
- [ ] NFSv4 支持（有状态协议，更好的并发和安全性）
- [ ] WebDAV 的 HTTP Basic Auth 集成
- [ ] 配置持久化（TOML/JSON 配置文件）
- [ ] 监控指标（Prometheus / tracing metrics）

---

## 四、快速开始

```bash
# 编译
cargo build --release

# 启动（NFS + WebDAV）
./target/release/nfsserve /data 0.0.0.0:2049 --dav-port 8080

# Linux 挂载 NFS
sudo mount -t nfs -o nolock,vers=3,tcp,port=2049,mountport=2049 127.0.0.1:/ /mnt/nfs

# Linux 挂载 WebDAV
sudo mount -t davfs http://127.0.0.1:8080/ /mnt/webdav

# curl 测试 WebDAV
curl -X PROPFIND http://127.0.0.1:8080/ -H "Depth: 1"
curl -X GET http://127.0.0.1:8080/test.txt
curl -X PUT http://127.0.0.1:8080/new.txt -d "hello"
```

---

## 五、仓库信息

- **Fork**: `https://github.com/qyqx233/nfsserve`
- **Upstream**: `https://github.com/huggingface/nfsserve`
- **当前 commit**: `4641848`
