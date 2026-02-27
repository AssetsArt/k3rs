# K3rs

**A lightweight container orchestration platform written in Rust.**

Inspired by [K3s](https://k3s.io/), powered by [Cloudflare Pingora](https://github.com/cloudflare/pingora), [Axum](https://docs.rs/axum), and [SlateDB](https://slatedb.io/).

```
┌─────────────────────────────────────────────────────────────┐
│  k3rs-server (Control Plane)                                │
│  ┌──────────┐ ┌───────────┐ ┌─────────────────────────────┐ │
│  │ API      │ │ Scheduler │ │ Controller Manager          │ │
│  │ (Axum)   │ │           │ │ (8 controllers)             │ │
│  └──────────┘ └───────────┘ └─────────────────────────────┘ │
│  ┌──────────┐ ┌───────────┐ ┌──────────┐                    │
│  │ SlateDB  │ │ Leader    │ │ PKI / CA │                    │
│  │ (S3/R2)  │ │ Election  │ │ (mTLS)   │                    │
│  └──────────┘ └───────────┘ └──────────┘                    │
└─────────────────────────────────────────────────────────────┘
        ▲                           ▲
        │  mTLS                     │  mTLS
        ▼                           ▼
┌──────────────────┐  ┌──────────────────┐
│  k3rs-agent      │  │  k3rs-agent      │
│  ┌────────────┐  │  │  ┌────────────┐  │
│  │ Container  │  │  │  │ Container  │  │
│  │ Runtime    │  │  │  │ Runtime    │  │
│  ├────────────┤  │  │  ├────────────┤  │
│  │ Service    │  │  │  │ Service    │  │
│  │ Proxy      │  │  │  │ Proxy      │  │
│  ├────────────┤  │  │  ├────────────┤  │
│  │ DNS Server │  │  │  │ DNS Server │  │
│  └────────────┘  │  │  └────────────┘  │
│  [Pod] [Pod]     │  │  [Pod] [Pod]     │
└──────────────────┘  └──────────────────┘
```

## Features

- **Pure Rust** — memory-safe, single binary per component
- **Control Plane / Data Plane** — server never touches containers; agents execute
- **Fail-Static** — restart server or agent without disrupting running pods
- **SlateDB** — embedded state store on object storage (S3/R2/MinIO), no etcd
- **Pingora** — L4/L7 service proxy, tunnel proxy, ingress controller
- **Platform-Aware Runtime** — Virtualization.framework microVMs (macOS), Firecracker (Linux), youki/crun OCI (Linux)
- **Management UI** — Dioxus 0.7 web dashboard
- **Backup/Restore** — online backup & restore via API, multi-server safe

## Quick Start

### Prerequisites

- **Rust** 1.93.1+ (edition 2024)
- **macOS** (primary dev platform) or **Linux**

### Run the Server

```bash
# Clone
git clone https://github.com/AssetsArt/k3rs.git
cd k3rs

# Start server (port 6443, local SlateDB)
cargo run --bin k3rs-server -- \
  --port 6443 \
  --token demo-token-123 \
  --data-dir /tmp/k3rs-data \
  --node-name master-1
```

### Run an Agent

```bash
cargo run --bin k3rs-agent -- \
  --server http://127.0.0.1:6443 \
  --token demo-token-123 \
  --node-name node-1
```

### CLI

```bash
# Cluster info
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 cluster info

# List nodes
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 node list

# Apply a manifest
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 apply -f manifest.yaml

# Get resources
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 get pods
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 get deployments
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 get services

# Logs & exec
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 logs <pod-name>
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 exec <pod-name> -- <command>

# Node operations
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 node drain <node-name>
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 node cordon <node-name>
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 node uncordon <node-name>

# Backup & restore
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 backup create --output ./backup.k3rs-backup.json.gz
cargo run --bin k3rsctl -- --server http://127.0.0.1:6443 restore --from ./backup.k3rs-backup.json.gz
```

## Development

### Dev Scripts (macOS)

```bash
# Full dev environment (tmux: server + UI)
./scripts/dev.sh

# Server only (cargo-watch auto-reload)
./scripts/dev-server.sh

# Agent only
./scripts/dev-agent.sh

# UI only (Dioxus)
./scripts/dev-ui.sh
```

### Dev with Podman (Linux — OCI & Firecracker)

For testing OCI runtimes (youki/crun) and Firecracker on Linux:

```bash
# Interactive shell with Linux environment
./scripts/dev-podman.sh

# Run server inside container
./scripts/dev-podman.sh server

# Run agent inside container
./scripts/dev-podman.sh agent

# Run tests
./scripts/dev-podman.sh test

# With KVM passthrough (Firecracker)
./scripts/dev-podman.sh --kvm shell

# With ALL
./scripts/dev-podman.sh --all --ui

# Environment variables
K3RS_RUNTIME=crun ./scripts/dev-podman.sh agent   # use crun instead of youki
K3RS_KVM=1 ./scripts/dev-podman.sh agent           # enable KVM
```

The Podman container includes: Rust toolchain, youki, crun, Firecracker, cargo-watch, and persistent cargo cache volumes.

### Build k3rs-init (Guest VM PID 1)

```bash
# Cross-compile from macOS → Linux musl (522KB static binary)
cargo zigbuild --release --target aarch64-unknown-linux-musl -p k3rs-init

# Or build kernel + initrd for microVMs
./scripts/build-kernel.sh
```

### Configuration

Both server and agent support YAML config files:

**Server** (`/etc/k3rs/config.yaml`):
```yaml
port: 6443
data-dir: /var/lib/k3rs/data
token: my-secret-token
```

**Agent** (`/etc/k3rs/agent-config.yaml`):
```yaml
server: https://10.0.0.1:6443
token: my-secret-token
node-name: worker-1
dns-port: 5353
```

Config precedence: **CLI flags** > **YAML config** > **defaults**

## Architecture

### Control Plane (k3rs-server)

The server is a **pure control plane** — it never runs containers:

| Component | Purpose |
|---|---|
| **API Server** (Axum) | REST API for all cluster operations |
| **Scheduler** | Resource-aware pod placement with affinity/taint support |
| **Controller Manager** | 8 controllers: Node, Deployment, ReplicaSet, DaemonSet, Job, CronJob, HPA, Eviction |
| **SlateDB** | Embedded KV store on object storage (S3/R2/local) |
| **Leader Election** | Lease-based HA — only leader runs controllers |
| **PKI / CA** | Issues mTLS certificates to agents |
| **Metrics** | Prometheus-compatible `/metrics` endpoint |

### Data Plane (k3rs-agent)

The agent runs on worker nodes and manages the container lifecycle:

| Component | Purpose |
|---|---|
| **Pod Sync Loop** | Watches scheduled pods → pull image → create → start → monitor |
| **Container Runtime** | Platform-aware: Virtualization.framework (macOS), Firecracker/youki/crun (Linux) |
| **Service Proxy** (Pingora) | L4/L7 load balancing, replaces kube-proxy |
| **Tunnel Proxy** (Pingora) | Persistent reverse tunnel to server |
| **DNS Server** | Resolves `<svc>.<ns>.svc.cluster.local` |
| **CNI** | Pod IP allocation from CIDR block |

### Container Runtime

| Platform | Backend | Technology |
|---|---|---|
| macOS | `VirtualizationBackend` | Apple Virtualization.framework microVMs |
| Linux (KVM) | `FirecrackerBackend` | Firecracker via rust-vmm crates |
| Linux (no KVM) | `OciBackend` | youki / crun OCI runtimes |

### State Store

All cluster state lives in SlateDB under structured key prefixes:

```
/registry/nodes/<name>                → Node metadata & status
/registry/pods/<ns>/<name>            → Pod spec & status
/registry/deployments/<ns>/<name>     → Deployment spec & status
/registry/services/<ns>/<name>        → Service definition
/registry/secrets/<ns>/<name>         → Secret data (encrypted)
/registry/configmaps/<ns>/<name>      → ConfigMap data
/events/<ns>/<timestamp>-<id>         → Cluster events (TTL-based)
```

### Fail-Static Guarantees

**Restart any component without disrupting running workloads.**

| Scenario | Running Containers | Service Proxy | DNS |
|---|---|---|---|
| **Server restart** | ✅ Unaffected | ✅ Continues | ✅ Continues |
| **Agent restart** | ✅ Independent processes | ❌→✅ Restarts | ❌→✅ Restarts |
| **Server + Agent restart** | ✅ Unaffected | ❌→✅ Restarts | ❌→✅ Restarts |

Key invariant: `kill -9 <agent-pid>` must **never** cause any container to stop.

## Workload Resources

| Resource | Description |
|---|---|
| **Pod** | Smallest deployable unit |
| **Deployment** | Manages ReplicaSets, rolling updates, blue/green, canary |
| **ReplicaSet** | Maintains N pod replicas |
| **DaemonSet** | One pod per node (or selected nodes) |
| **Job** | Run-to-completion workload |
| **CronJob** | Scheduled jobs |
| **Service** | ClusterIP / NodePort / LoadBalancer |
| **Ingress** | Host/path-based external routing |
| **ConfigMap** | Configuration data |
| **Secret** | Sensitive data (encrypted at rest) |
| **HPA** | Horizontal Pod Autoscaler |
| **NetworkPolicy** | Pod-level network isolation |
| **ResourceQuota** | Per-namespace limits |
| **PVC** | Persistent volume claims |

## Security

- **mTLS everywhere** — Server ↔ Agent, auto-rotated certificates
- **Join token** — Agents register with pre-shared token, receive TLS cert
- **RBAC** — `cluster-admin`, `namespace-admin`, `viewer` roles
- **Service accounts** — Scoped tokens for workload API access
- **Secrets encrypted at rest** in SlateDB

## Project Structure

```
k3rs/
├── cmd/
│   ├── k3rs-server/       # Control plane binary
│   ├── k3rs-agent/        # Data plane binary
│   ├── k3rs-init/         # Guest PID 1 for microVMs (static musl binary, 522KB)
│   ├── k3rs-vmm/          # Virtualization.framework helper (macOS)
│   ├── k3rs-ui/           # Management UI (Dioxus 0.7)
│   └── k3rsctl/           # CLI tool
├── pkg/
│   ├── api/               # Axum HTTP API & handlers
│   ├── container/         # Container runtime (Virtualization/Firecracker/OCI)
│   ├── controllers/       # 8 control loops
│   ├── metrics/           # Prometheus metrics registry
│   ├── network/           # CNI + DNS
│   ├── pki/               # CA & mTLS certificates
│   ├── proxy/             # Pingora proxies (Service/Ingress/Tunnel)
│   ├── scheduler/         # Pod placement logic
│   ├── state/             # SlateDB integration
│   └── types/             # Cluster object models
├── scripts/
│   ├── dev.sh             # Full dev environment (tmux)
│   ├── dev-agent.sh       # Agent dev loop
│   ├── dev-podman.sh      # Podman Linux dev environment
│   ├── build-kernel.sh    # Build Linux kernel + initrd for microVMs
│   └── ...
├── Containerfile.dev      # Podman dev image (Rust + youki + crun + Firecracker)
├── Cargo.toml             # Workspace root (14 crates)
└── spec.md                # Full specification
```

## Tech Stack

| Category | Technology |
|---|---|
| Language | Rust (edition 2024) |
| HTTP API | Axum 0.8 |
| Proxy / Networking | Cloudflare Pingora 0.7 |
| State Store | SlateDB 0.10 on object storage |
| Management UI | Dioxus 0.7 (WASM SPA) |
| Container Runtime | Virtualization.framework / Firecracker / youki / crun |
| Image Pull | oci-client (OCI Distribution spec) |
| DNS | Hickory DNS |
| Serialization | serde + serde_json |
| Async Runtime | Tokio |
| CLI | Clap |
| TLS | rustls + rcgen |
| Telemetry | OpenTelemetry + OTLP |

## License

MIT
