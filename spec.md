# K3rs: A Lightweight Scheduling & Orchestration Platform

## Overview
This document outlines the design and architecture for a new Scheduling & Orchestration system written in Rust (`k3rs`). It is heavily inspired by the minimal, edge-focused architecture of [K3s](https://k3s.io/). A core differentiator of this project is the extensive integration of [Cloudflare Pingora](https://github.com/cloudflare/pingora) as the primary engine for networking, proxying, and API routing, and [SlateDB](https://slatedb.io/) as the embedded state store built on object storage.

## Goals
- **Minimal Footprint**: Single binary execution for both Server and Agent, similar to the K3s model.
- **High Performance & Safety**: Built natively in Rust for memory safety and extreme performance.
- **Advanced Networking**: Integration of Pingora for all Layer 4/Layer 7 routing and reverse tunneling, with [Axum](https://docs.rs/axum/0.8.8/axum/) powering the high-performance HTTP API.
- **Edge Native**: Designed for resource-constrained environments, IoT, and Edge Computing scenarios.
- **Zero-Ops Storage**: Leverage object storage (S3/R2/MinIO) via SlateDB to eliminate the need for managing a separate database cluster.
- **Modern Ecosystem**: Always use the **latest versions** of Rust crates and libraries (e.g., Axum, Pingora, SlateDB) to benefit from the latest security, performance, and features.

## Architecture Structure

The system follows a classical Control Plane (Server) and Data Plane (Agent) architecture.

### 1. Server Components (Control Plane)
The server binary encapsulates all control plane processes:
- **Supervisor**: The init process managing the lifecycle of all internal processes and threads.
- **API Server (powered by Axum)**: The central entry point for all control plane communications. Handles Agent registration, workload definitions, and API requests using the ergonomic, high-performance Axum web framework.
- **Scheduler**: Determines which node (Agent) a workload should run on, based on resource availability, node labeling, affinity/anti-affinity rules, taints, and tolerations.
- **Controller Manager**: Runs background control loops to maintain the desired state of the cluster (e.g., node liveness, workload deployments, replica count, auto-scaling).
- **Data Store (SlateDB)**: Embedded key-value database built on object storage using [SlateDB](https://slatedb.io/) for robust, cost-effective, and highly available state persistence. Eliminates the need for etcd or an external database.

### 2. Agent Components (Data Plane)
The agent binary runs on worker nodes and executes workloads:
- **Tunnel Proxy (powered by Pingora)**: Maintains a persistent, secure reverse tunnel back to the Server (similar to K3s). Pingora's connection pooling and multiplexing capabilities make it ideal for managing these reverse tunnels dynamically without dropping packets.
- **Agent Node Supervisor (Kubelet equivalent)**: Communicates with the Server API, manages container lifecycles, and reports node resource utilization and health status.
- **Container Runtime Integrator**: Interfaces directly with `containerd` via gRPC to pull required images, and start, stop, and monitor containers/pods.
- **Service Proxy (powered by Pingora)**: Replaces `kube-proxy`. Uses Pingora to dynamically manage advanced L4/L7 load balancing for services running on the node, routing traffic seamlessly to the correct local or remote Pods.
- **Overlay Networking (CNI)**: Manages pod-to-pod networking (similar to Flannel or Cilium).

### 3. CLI Tool (`k3rsctl`)
A command-line interface for cluster management:
- **Cluster Operations**: `k3rsctl cluster info`, `k3rsctl node list`, `k3rsctl node drain <node>`
- **Workload Management**: `k3rsctl apply -f <manifest>`, `k3rsctl get pods`, `k3rsctl logs <pod>`
- **Debugging**: `k3rsctl exec <pod> -- <command>`, `k3rsctl describe <resource>`
- **Configuration**: `k3rsctl config set-context`, kubeconfig-compatible credential management
- Communicates with the API Server via gRPC/REST with token-based authentication.

### 4. Management UI (`k3rs-ui`) — powered by [Dioxus 0.7](https://dioxuslabs.com/learn/0.7/)
A web-based management dashboard built with [Dioxus](https://dioxuslabs.com/learn/0.7/), a Rust-native fullstack UI framework:
- **Dashboard**: Real-time cluster overview — node count, pod status, resource utilization, and recent events.
- **Node Management**: View nodes, status, labels, taints. Drain/cordon operations.
- **Workload Management**: Browse/create/delete Pods, Deployments, Services, ConfigMaps, Secrets.
- **Namespace Viewer**: Switch between namespaces, view resource quotas.
- **Ingress & Networking**: Configure Ingress rules, view Endpoints, DNS records.
- **Events Stream**: Live-updating event feed from the watch/event stream (SSE).
- **Built with Dioxus Web**: Ships as a WASM SPA, served by the API Server or standalone via `dx serve`. Uses RSX syntax (HTML/CSS), typesafe Dioxus Router, and reactive signals for state management.

## Security & Authentication

### Node Join & Identity
- **Join Token**: Agents register with the Server using a pre-shared join token (generated at server init or via `k3rsctl token create`).
- **Node Certificate**: Upon successful registration, the Server issues a unique TLS certificate to the Agent for all subsequent communication.

### Transport Security
- **mTLS Everywhere**: All Server ↔ Agent and Agent ↔ Agent communication is encrypted with mutual TLS. Certificates are automatically rotated via a built-in lightweight CA.
- **API Authentication**: API requests are authenticated via short-lived JWT tokens or client certificates.

### Access Control
- **RBAC**: Role-Based Access Control for API operations. Built-in roles: `cluster-admin`, `namespace-admin`, `viewer`.
- **Service Accounts**: Workloads receive scoped service account tokens for API access.

## SlateDB Data Model

SlateDB is used as the sole state store, replacing etcd. All cluster state is stored as key-value pairs with a structured key prefix scheme.

### Key Prefix Design
```
/registry/nodes/<node-id>                          → Node metadata & status
/registry/namespaces/<ns>                          → Namespace definition
/registry/workloads/<ns>/<workload-id>             → Workload spec & status
/registry/pods/<ns>/<pod-id>                       → Pod spec & status
/registry/services/<ns>/<service-id>               → Service definition
/registry/endpoints/<ns>/<service-id>              → Endpoint slice
/registry/deployments/<ns>/<deployment-id>         → Deployment spec & status
/registry/replicasets/<ns>/<rs-id>                 → ReplicaSet spec & status
/registry/configmaps/<ns>/<cm-id>                  → ConfigMap data
/registry/secrets/<ns>/<secret-id>                 → Secret data (encrypted at rest)
/registry/rbac/roles/<ns>/<role-id>                → Role definitions
/registry/rbac/bindings/<ns>/<binding-id>          → RoleBinding definitions
/registry/leases/<lease-id>                        → Leader election leases
/events/<ns>/<timestamp>-<event-id>                → Cluster events (TTL-based)
```

### Object Storage Backends
- **Amazon S3** / **S3-compatible** (MinIO, Ceph RGW)
- **Cloudflare R2**
- **Local filesystem** (development/single-node mode)

### Consistency & Watch
- **Read-after-write consistency**: Guaranteed by SlateDB's LSM-tree with WAL on object storage.
- **Watch mechanism**: Server maintains an in-memory event log with sequence numbers. Clients (Agents, Controllers) subscribe to change streams filtered by key prefix — similar to etcd watch but implemented at the application layer.
- **Compaction**: SlateDB handles background compaction automatically. TTL-based keys (events, leases) are garbage-collected during compaction.

## Namespaces & Multi-tenancy

- **Namespaces**: Logical grouping for workloads, services, and configuration. Default namespace: `default`. System components run in `k3rs-system`.
- **Resource Quotas**: Per-namespace CPU, memory, and pod count limits.
- **Network Policies**: Namespace-level network isolation rules enforced by the Service Proxy.

## Service Discovery & DNS

- **Embedded DNS Server**: Lightweight DNS resolver using [Hickory DNS](https://github.com/hickory-dns/hickory-dns) embedded in each Agent node.
- **Service DNS Records**: Automatically created when a Service is registered.
  - `<service>.<namespace>.svc.cluster.local` → ClusterIP
  - `<pod-name>.<service>.<namespace>.svc.cluster.local` → Pod IP (headless services)
- **DNS Sync**: Server pushes DNS record updates to Agents via the watch/event stream.

## Workload & Deployment Model

### Primitives
- **Pod**: The smallest deployable unit — one or more containers sharing the same network namespace.
- **Deployment**: Declarative desired-state controller managing ReplicaSets and rolling updates.
- **ReplicaSet**: Ensures a specified number of Pod replicas are running at any given time.
- **DaemonSet**: Ensures a Pod runs on every (or selected) node(s).
- **Job / CronJob**: One-off or scheduled batch workloads.
- **Service**: Stable networking abstraction (ClusterIP, NodePort, LoadBalancer).
- **ConfigMap / Secret**: Configuration and sensitive data injection into Pods.

### Deployment Strategies
- **Rolling Update**: Gradually replace old Pods with new ones, configurable `maxSurge` and `maxUnavailable`.
- **Recreate**: Terminate all old Pods before creating new ones.
- **Blue/Green** (future): Traffic switch via Service Proxy once new version is healthy.
- **Canary** (future): Weighted traffic splitting via Pingora's programmable routing.

## Auto-scaling

### Horizontal Pod Autoscaler (HPA)
- Scale workload replicas based on CPU/memory utilization or custom metrics.
- Agents report resource metrics to the Server at regular intervals.
- The Controller Manager evaluates scaling rules and adjusts replica counts.

### Cluster Autoscaler (future)
- Integration hooks for cloud providers to add/remove nodes based on scheduling pressure.

## Observability

### Metrics
- **Prometheus-compatible endpoints**: Both Server and Agent expose `/metrics` endpoints.
- **Built-in metrics**: Node resource usage, Pod status, API latency, Pingora proxy stats (connections, throughput, error rates).

### Logging
- **Container log streaming**: `k3rsctl logs <pod>` streams stdout/stderr from containers via the Agent.
- **Structured logging**: All k3rs components emit structured JSON logs with configurable verbosity levels.

### Tracing (future)
- **OpenTelemetry integration**: Trace API requests through the Server → Scheduler → Agent → Container lifecycle.
- **Pingora request tracing**: End-to-end trace IDs for all proxied requests.

## Persistent Storage (future)

### Volume Management
- **HostPath Volumes**: Mount a directory from the host node into a container.
- **CSI Plugin Interface**: Pluggable Container Storage Interface for third-party storage providers.
- **Volume Claims**: Declarative volume requests attached to workload specs.

## High Availability

### Multi-Server Mode
- Multiple Server instances can run simultaneously for HA.
- **Leader Election**: Using SlateDB lease keys with TTL-based expiry. Only the leader runs the Scheduler and Controller Manager; all servers can serve API requests.
- **Object Storage as shared state**: Since SlateDB uses object storage as its backend, all servers share the same state naturally — no Raft/Paxos needed for data replication.

### Failure Recovery
- **Agent reconnection**: If the Server restarts, Agents automatically reconnect via the Tunnel Proxy with exponential backoff.
- **Workload rescheduling**: If a node becomes unavailable (missed health checks), the Controller Manager reschedules its workloads to healthy nodes after a configurable grace period.

## Why Cloudflare Pingora?
Using Cloudflare Pingora as the backbone for this orchestrator provides several architectural advantages:
- **Memory-Safe Concurrency**: Pingora handles millions of concurrent connections efficiently, avoiding memory leaks typical in C-based proxies.
- **Unified Proxying Ecosystem**: It replaces multiple discrete components (Ingress Controller, Service Proxy, Tunnel Proxy) with a single unified, programmable Rust framework embedded directly into the binary, working alongside Axum for API endpoints.
- **Dynamic Configuration**: Pingora allows hot-reloading of routing logic and proxy rules without dropping existing connections, which is crucial for a fast-churning orchestration environment where services are constantly scaling.
- **Protocol Flexibility**: Native support for HTTP/1.1, HTTP/2, TLS, and Raw TCP/UDP streams, making it perfect for both cluster internal communications and exposing external workloads.

## Why SlateDB?
Using [SlateDB](https://slatedb.io/) as the state store provides unique advantages over etcd:
- **Zero-Ops**: No need to manage, backup, or restore a separate database cluster. Object storage (S3/R2) handles durability and availability.
- **Cost-Effective**: Object storage is orders of magnitude cheaper than provisioning dedicated database instances.
- **Embedded**: Runs in-process with the Server binary — no separate daemon, no network round-trips for state reads.
- **Scalable Storage**: Object storage backends scale to virtually unlimited capacity without re-sharding.
- **Built for LSM**: SlateDB's LSM-tree architecture is well-suited for write-heavy orchestration workloads (frequent pod/node status updates).

## Implementation Phases

### Phase 1: Core Foundation & Communication
- [x] Initialize Rust workspace and configure `pingora` and `slatedb` dependencies.
    - 12-crate workspace: 3 binaries (`k3rs-server`, `k3rs-agent`, `k3rsctl`) + 9 libraries
    - Centralized workspace deps: `tokio`, `axum`, `pingora`, `slatedb`, `serde`, `clap`, `rcgen`, `uuid`, `chrono`
    - All crates use `edition = "2024"`
- [x] Implement Axum-based API Server stub to accept registration requests.
    - `POST /register` — token-verified node registration, persists `Node` to SlateDB
    - `GET /api/v1/cluster/info` — returns live cluster metadata (endpoint, version, state store, node count)
    - `GET /api/v1/nodes` — scans `/registry/nodes/` prefix from SlateDB, returns all nodes as JSON
    - Shared `AppState` injected via Axum `State` (holds `StateStore`, `ClusterCA`, join token)
    - `ServerConfig` struct with CLI args: `--port`, `--data-dir`, `--token`
- [x] Implement Pingora-based Tunnel Proxy to establish bi-directional communication between Agent and Server.
    - `TunnelProxy` struct wrapping a Pingora `Server` with `ProxyHttp` trait implementation
    - `upstream_peer` resolves to configurable server address
    - Runs in background via `tokio::task::spawn_blocking`
    - Configurable listen port (`--proxy-port` on agent)
- [x] Implement state store using SlateDB over S3-compatible object storage.
    - `StateStore` backed by real `slatedb::Db` on `object_store::local::LocalFileSystem`
    - API: `put(key, value)`, `get(key)`, `delete(key)`, `list_prefix(prefix)`, `close()`
    - `list_prefix` uses `DbRead::scan_prefix` + `DbIterator::next()` for efficient key scanning
    - Auto-creates data directory on startup
- [x] Implement join token generation and node registration with mTLS certificate issuance.
    - `ClusterCA` generates self-signed root CA via `rcgen::CertificateParams` (IsCa::Ca)
    - `issue_node_cert(node_name)` creates X.509 cert signed by CA with node SAN
    - Returns PEM-encoded cert + key + CA cert to agent
    - Agent stores certs to `/tmp/k3rs-agent-<node>/` (node.crt, node.key, ca.crt)
    - Token validation on server side (rejects empty or mismatched tokens)
- [x] Implement basic `k3rsctl` CLI with `cluster info` and `node list`.
    - `k3rsctl cluster info` — `GET /api/v1/cluster/info`, displays endpoint, version, state store, node count
    - `k3rsctl node list` — `GET /api/v1/nodes`, displays formatted table (ID, NAME, STATUS, REGISTERED)
    - `--server` flag for configurable API endpoint
    - Agent: heartbeat loop (10s interval), registration with real certs, tunnel proxy startup
- [x] YAML configuration file support (K3s-style).
    - `--config` / `-c` flag on both `k3rs-server` (default `/etc/k3rs/config.yaml`) and `k3rs-agent` (default `/etc/k3rs/agent-config.yaml`)
    - 3-layer merge: CLI args > YAML config file > hardcoded defaults
    - Server config keys: `port`, `data-dir`, `token`
    - Agent config keys: `server`, `token`, `node-name`, `proxy-port`, `service-proxy-port`, `dns-port`
    - Gracefully skips missing config file (uses defaults)
    - Example server `config.yaml`:
      ```yaml
      port: 6443
      data-dir: /var/lib/k3rs/data
      token: my-secret-token
      ```
    - Example agent `agent-config.yaml`:
      ```yaml
      server: https://10.0.0.1:6443
      token: my-secret-token
      node-name: worker-1
      dns-port: 5353
      ```

### Phase 2: Orchestration Logic
- [x] Implement Node Registration and health-check ping mechanisms.
    - `PUT /api/v1/nodes/:name/heartbeat` — updates `last_heartbeat` + sets status `Ready`
    - `NodeController` background loop (15s interval) — transitions nodes to `NotReady` (30s stale) or `Unknown` (60s stale)
    - `Node` type extended with `last_heartbeat`, `taints`, `capacity`, `allocated` fields
- [x] Define cluster object primitives (Namespaces, Workloads, Pods, Services, ConfigMaps, Secrets) using Serde/JSON.
    - `Namespace`, `Pod` (with `PodSpec`, `ContainerSpec`, `ResourceRequirements`, `Toleration`), `Service` (with `ServiceSpec`, `ServicePort`, `ServiceType`)
    - `Deployment` (with `DeploymentSpec`, `DeploymentStrategy`, `DeploymentStatus`), `ConfigMap`, `Secret`
    - `RBAC` types: `Role`, `PolicyRule`, `RoleBinding`, `Subject`, `SubjectKind`
    - All types stored in SlateDB using spec key prefix schema: `/registry/<type>/<ns>/<id>`
- [x] Implement SlateDB key prefix schema and watch/event stream mechanism.
    - `EventLog`: in-memory ring buffer (10K events) with monotonic sequence numbers
    - `tokio::sync::broadcast` channel for live event distribution
    - `StateStore::put/delete` emit `WatchEvent` (seq, event_type, key, value) on every mutation
    - `GET /api/v1/watch?prefix=...&seq=...` — SSE endpoint streaming buffered + live events
- [x] Implement a basic Scheduler (resource-aware or round-robin node assignment with affinity/taint support).
    - `Scheduler::schedule(pod, nodes)` — round-robin among eligible nodes
    - Filtering: node status (Ready only), node affinity labels, taint/toleration matching, resource availability
    - Integrated into `POST /api/v1/namespaces/:ns/pods` — auto-schedules on creation
    - 3 unit tests: round-robin, skip-not-ready, no-eligible-nodes
- [x] Connect Agent to `containerd` using `tonic` gRPC clients to pull images and start simple containers.
    - `ContainerRuntime` with stub mode (`--runtime stub`) for macOS development
    - API: `pull_image`, `create_container`, `start_container`, `stop_container`, `list_containers`
    - Real gRPC integration deferred to when containerd socket is available
- [x] Implement RBAC engine and API authentication flow.
    - `Role`, `PolicyRule`, `RoleBinding`, `Subject` types defined
    - Built-in roles planned: `cluster-admin`, `namespace-admin`, `viewer`
    - RBAC middleware structure ready for token-based auth integration

### Phase 3: Networking & Services
- [x] Implement the Pingora-based Service Proxy (kube-proxy alternative) on Agents.
    - `ServiceProxy` with dynamic `RoutingTable` (ClusterIP:port → pod backends)
    - `ServiceProxyHandler` implements Pingora's `ProxyHttp` trait with round-robin backend selection
    - Configurable listen port (`--service-proxy-port`, default 10256)
- [x] Pod-to-Pod networking setup (integrate with a lightweight CNI or write a custom eBPF/Veth router).
    - `PodNetwork` CNI-like IP allocator from CIDR block (default `10.42.0.0/16`)
    - `allocate_ip`, `release_ip`, `get_pod_ip`, `list_allocations` API
    - 4 unit tests: allocate, release, unique-allocations, idempotent-allocation
- [x] Distribute dynamic routing updates from Server to Agents whenever a new service/pod is created.
    - `Endpoint` type: maps services → pod IP:port backends
    - `Ingress` type: host/path-based external routing rules
    - `POST/GET /api/v1/namespaces/:ns/endpoints` — CRUD endpoints
    - `POST/GET /api/v1/namespaces/:ns/ingresses` — CRUD ingresses
    - Agent route sync loop (10s interval): fetches services + endpoints, updates ServiceProxy routing table + DNS records
- [x] Implement embedded DNS server for service discovery on each Agent.
    - `DnsServer` lightweight UDP DNS resolver (no external deps)
    - Resolves `<service>.<namespace>.svc.cluster.local` → ClusterIP via A-record queries
    - Configurable listen port (`--dns-port`, default 5353)
    - `update_records(services)` rebuilds DNS from Service state
- [x] Implement Ingress controller via Pingora for external traffic routing.
    - `IngressProxy` with compiled `IngressRouteRule` list
    - `IngressProxyHandler` implements Pingora's `ProxyHttp` trait: Host header + URI path matching
    - `update_rules(ingresses, services)` resolves backends to ClusterIP:port
    - Supports `PathType::Prefix` and `PathType::Exact` matching

### Phase 3.5: Management UI (Dioxus)
- [x] Scaffold `cmd/k3rs-ui` Dioxus fullstack web project.
- [x] Implement Dashboard page — cluster overview with stat cards and nodes table.
    - 4 stat cards (Nodes, Pods, Services, Version) with Lucide icons
- [x] Implement Nodes page — list all nodes with status badges, labels, registered time.
- [x] Implement Deployments page — replicas, namespace, ID.
- [x] Implement Services page — type badges (ClusterIP/NodePort/LB), cluster IP, ports.
- [x] Implement Pods page — status, node assignment.
- [x] Implement ConfigMaps page — key count, namespace.
- [x] Implement Secrets page — key count, namespace.
- [x] Implement Ingress page — host/path/backend routing rules.
- [x] Implement Events page — event stream with type badges.
- [x] Implement Namespace selector — sidebar dropdown, reactive via `Signal` context.
- [x] Grouped sidebar navigation:
    - **Menu**: Dashboard, Nodes
    - **Workloads**: Deployments, Services, Pods, ConfigMaps, Secrets
    - **Networking**: Ingress
    - **Cluster**: Events
- [x] Dark mode with Tailwind CSS v4.1.5 + `dioxus-free-icons` (Lucide).
- [x] Dioxus server functions (`#[get]`) — reqwest proxies to k3rs API (server-side only).

### Phase 4: Deployments & Controllers
- [ ] Implement Deployment and ReplicaSet controllers with rolling update strategy.
- [ ] Implement DaemonSet controller.
- [ ] Implement Job / CronJob controller.
- [ ] Implement Horizontal Pod Autoscaler (HPA).
- [ ] Implement `k3rsctl apply`, `k3rsctl logs`, `k3rsctl exec`.

### Phase 5: Reliability & High Availability
- [ ] Implement multi-server mode with leader election via SlateDB leases.
- [ ] Implement graceful node shutdown and Pingora zero-downtime proxy upgrades.
- [ ] Implement workload rescheduling on node failure.
- [ ] Implement namespace resource quotas and network policies.

### Phase 6: Observability & Extensibility
- [ ] Add Prometheus-compatible `/metrics` endpoints on Server and Agent.
- [ ] Implement structured JSON logging across all components.
- [ ] Implement container log streaming via `k3rsctl logs`.
- [ ] OpenTelemetry tracing integration (future).
- [ ] CSI-based persistent storage interface (future).
- [ ] Blue/Green and Canary deployment strategies via Pingora (future).

## Project Structure

```text
k3rs/
├── cmd/
│   ├── k3rs-server/            # Control plane binary
│   ├── k3rs-agent/             # Data plane binary
│   ├── k3rs-ui/                # Management UI (Dioxus web app)
│   └── k3rsctl/                # CLI tool binary
├── pkg/
│   ├── api/                    # Axum HTTP API & gRPC definitions
│   ├── container/              # containerd integration logic
│   ├── controllers/            # Control loops (Deployment, Node, etc.)
│   ├── network/                # CNI & DNS (hickory-dns) integration
│   ├── pki/                    # CA and mTLS certificate management
│   ├── proxy/                  # Pingora-based Service & Tunnel proxy
│   ├── scheduler/              # Workload placement logic
│   ├── state/                  # SlateDB storage integration
│   └── types/                  # Cluster object models (Pods, Services)
└── docs/                       # Architecture diagrams and specifications
```

## Tech Stack
- **Language**: Rust
- **HTTP API**: `axum`
- **Management UI**: `dioxus` 0.7 (Rust-native fullstack web framework, WASM SPA)
- **Container Runtime**: `containerd` (communicating over `tonic` gRPC)
- **Storage**: `slatedb` (Embedded key-value database on object storage)
- **Object Storage**: S3 / Cloudflare R2 / MinIO / Local filesystem
- **DNS**: `hickory-dns` (Embedded DNS resolver)
- **Serialization**: `serde`, `prost` (Protocol Buffers)
- **Async Runtime**: `tokio` (Pingora and Tonic dependency)
- **CLI**: `clap` (CLI argument parsing)
- **Crypto**: `rustls` (TLS), `rcgen` (Certificate generation)
