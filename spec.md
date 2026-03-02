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

The system follows a classical **Control Plane (Server)** and **Data Plane (Agent)** architecture with strict separation of concerns. The Server **does not** run containers — all container lifecycle management is performed by the Agent.

> **Fail-Static Principle**: Restarting or crashing any component must **never** disrupt running workloads. Containers continue to run on Agent nodes regardless of Server or Agent process state. See [Fail-Static Guarantees](#fail-static-guarantees) for the full specification.

```mermaid
graph TB
    subgraph server["k3rs-server (Control Plane)"]
        direction TB
        API["API Server<br/>(Axum)"]
        SCHED["Scheduler"]
        CTRL["Controller Manager<br/>(8 controllers)"]
        DB["Data Store<br/>(SlateDB)"]
        LEADER["Leader Election"]
        METRICS["Metrics<br/>(/metrics)"]
        PKI["PKI / CA<br/>(mTLS)"]

        API <--> DB
        SCHED <--> DB
        CTRL <--> DB
        LEADER --> CTRL
        LEADER --> SCHED
        API --> PKI
    end

    subgraph agent["k3rs-agent (Data Plane)"]
        direction TB
        KUBELET["Pod Sync Loop<br/>(Kubelet equivalent)"]
        RUNTIME["Container Runtime<br/>(Virtualization / OCI)"]
        SPROXY["Service Proxy<br/>(Pingora)"]
        TUNNEL["Tunnel Proxy<br/>(Pingora)"]
        DNS["DNS Server<br/>(svc.cluster.local)"]
        CNI["CNI<br/>(Pod Networking)"]

        KUBELET --> RUNTIME
        RUNTIME --> PODS
        SPROXY <--> PODS
        DNS --> SPROXY
        CNI --> PODS
    end

    PODS["Pods"]

    CLI["k3rsctl (CLI)"] --> API
    UI["k3rs-ui (Dioxus)"] --> API
    TUNNEL <--> API
    KUBELET <--> API

    style server fill:#1a1a2e,stroke:#e94560,stroke-width:2px,color:#fff
    style agent fill:#16213e,stroke:#0f3460,stroke-width:2px,color:#fff
    style PODS fill:#533483,stroke:#e94560,color:#fff
```

### 1. Server Components (Control Plane)
The server binary encapsulates **only** control plane processes. It does not run containers or manage container runtimes:
- **API Server (powered by Axum)**: The central entry point for all control plane communications. Handles Agent registration, workload definitions, and API requests using the ergonomic, high-performance Axum web framework.
- **Scheduler**: Determines which node (Agent) a workload should run on, based on resource availability, node labeling, affinity/anti-affinity rules, taints, and tolerations.
- **Controller Manager**: Runs background control loops to maintain the desired state of the cluster (e.g., node liveness, workload deployments, replica count, auto-scaling). Controllers only manage desired state — they create/delete Pod records, but the Agent is responsible for the actual container lifecycle.
- **Data Store (SlateDB)**: Embedded key-value database built on object storage using [SlateDB](https://slatedb.io/) for robust, cost-effective, and highly available state persistence. Eliminates the need for etcd or an external database.
- **Leader Election**: Ensures only one Server runs Scheduler + Controllers in multi-server HA mode.
- **PKI / CA**: Issues mTLS certificates to Agents on registration.

### 2. Agent Components (Data Plane)
The agent binary runs on worker nodes and executes workloads:
- **Tunnel Proxy (powered by Pingora)**: Maintains a persistent, secure reverse tunnel back to the Server (similar to K3s). Pingora's connection pooling and multiplexing capabilities make it ideal for managing these reverse tunnels dynamically without dropping packets.
- **Pod Sync Loop (Kubelet equivalent)**: Watches for Scheduled pods, pulls images, creates and starts containers, monitors health, and reports status back to the Server API.
- **Container Runtime Integrator**: Platform-aware container runtime with pluggable backends — Virtualization.framework microVM on macOS (Firecracker-like lightweight Linux VMs), OCI runtimes (`youki`/`crun`) with auto-download from GitHub Releases on Linux. Pulls OCI images via `oci-client`, extracts rootfs layers, boots minimal Linux VMs or OCI containers, and manages full container lifecycle including exec.
- **Service Proxy (powered by Pingora)**: Replaces `kube-proxy`. Uses Pingora to dynamically manage advanced L4/L7 load balancing for services running on the node, routing traffic seamlessly to the correct local or remote Pods.
- **DNS Server**: Lightweight embedded DNS resolver for `<service>.<namespace>.svc.cluster.local` resolution.
- **Overlay Networking (CNI)**: Manages pod-to-pod networking and IP allocation (similar to Flannel or Cilium).

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

All resource keys use **name** as the primary identifier (K8s-style), not UUID.
Names must be `[a-z0-9-]`, max 63 characters, no leading/trailing hyphens (RFC 1123).
UUIDs are stored as `.id` fields for internal reference only.

```
/registry/nodes/<node-name>                          → Node metadata & status
/registry/namespaces/<ns>                             → Namespace definition
/registry/pods/<ns>/<pod-name>                        → Pod spec & status
/registry/services/<ns>/<service-name>                → Service definition
/registry/endpoints/<ns>/<endpoint-name>              → Endpoint slice
/registry/ingresses/<ns>/<ingress-name>               → Ingress routing rules
/registry/deployments/<ns>/<deployment-name>          → Deployment spec & status
/registry/replicasets/<ns>/<rs-name>                  → ReplicaSet spec & status
/registry/daemonsets/<ns>/<ds-name>                   → DaemonSet spec & status
/registry/jobs/<ns>/<job-name>                        → Job spec & status
/registry/cronjobs/<ns>/<cj-name>                     → CronJob spec & status
/registry/configmaps/<ns>/<cm-name>                   → ConfigMap data
/registry/secrets/<ns>/<secret-name>                  → Secret data (encrypted at rest)
/registry/hpa/<ns>/<hpa-name>                         → Horizontal Pod Autoscaler
/registry/resourcequotas/<ns>/<quota-name>            → Namespace resource quota
/registry/networkpolicies/<ns>/<policy-name>          → Network policy
/registry/pvcs/<ns>/<pvc-name>                        → Persistent volume claim
/registry/images/<node-name>                          → Per-node image list
/registry/leases/controller-leader                    → Leader election lease
```

> [!NOTE]
> - RBAC keys (`/registry/rbac/roles/`, `/registry/rbac/bindings/`) are referenced in the auth middleware but not yet persisted — RBAC enforcement is currently done with hardcoded built-in roles.
> - Events are stored in an in-memory ring buffer (`EventLog`, 10K events) with `tokio::sync::broadcast`, not in the key-value store.

#### Name Validation (`pkg/types/src/validate.rs`)
- [x] `validate_name(name) -> Result<()>` — `[a-z0-9-]`, max 63 chars, no leading/trailing `-`
- [x] Resource uniqueness: `(namespace, name)` pair must be unique
- [x] Unit tests: valid names + invalid names (uppercase, underscore, leading hyphen, too long)

### Object Storage Backends
- **Amazon S3** / **S3-compatible** (MinIO, Ceph RGW)
- **Cloudflare R2**
- **Local filesystem** (development/single-node mode)

### Consistency & Watch
- **Read-after-write consistency**: Guaranteed by SlateDB's LSM-tree with WAL on object storage.
- **Watch mechanism**: Server maintains an in-memory event log with sequence numbers. Clients (Agents, Controllers) subscribe to change streams filtered by key prefix — similar to etcd watch but implemented at the application layer.
- **Compaction**: SlateDB handles background compaction automatically. TTL-based keys (leases) are garbage-collected during compaction.

## API Reference

### Public (Unauthenticated)

| Method | Path | Handler | Description |
|--------|------|---------|-------------|
| `POST` | `/register` | `register::register_node` | Agent join with token → receive mTLS cert |
| `GET` | `/api/v1/cluster/info` | `cluster::cluster_info` | Cluster metadata (endpoint, version, node count) |
| `GET` | `/metrics` | `metrics_handler` | Prometheus text exposition |

### Protected (Authenticated + RBAC)

**Nodes**

| Method | Path | Handler | Description |
|--------|------|---------|-------------|
| `GET` | `/api/v1/nodes` | `cluster::list_nodes` | List all nodes |
| `PUT` | `/api/v1/nodes/{name}/heartbeat` | `heartbeat::node_heartbeat` | Agent heartbeat |
| `GET` | `/api/v1/nodes/{name}/pods` | `resources::list_node_pods` | List pods on a node (all namespaces) |
| `POST` | `/api/v1/nodes/{name}/cordon` | `drain::cordon_node` | Mark node unschedulable |
| `POST` | `/api/v1/nodes/{name}/uncordon` | `drain::uncordon_node` | Remove unschedulable flag |
| `POST` | `/api/v1/nodes/{name}/drain` | `drain::drain_node` | Cordon + evict all pods |
| `PUT` | `/api/v1/nodes/{name}/images` | `images::report_node_images` | Agent reports per-node images |

**Namespaces**

| Method | Path | Handler |
|--------|------|--------|
| `POST` | `/api/v1/namespaces` | `resources::create_namespace` |
| `GET` | `/api/v1/namespaces` | `resources::list_namespaces` |

**Pods**

| Method | Path | Handler |
|--------|------|--------|
| `POST` | `/api/v1/namespaces/{ns}/pods` | `resources::create_pod` |
| `GET` | `/api/v1/namespaces/{ns}/pods` | `resources::list_pods` |
| `GET` | `/api/v1/namespaces/{ns}/pods/{pod_name}` | `resources::get_pod` |
| `DELETE` | `/api/v1/namespaces/{ns}/pods/{pod_name}` | `resources::delete_pod` |
| `PUT` | `/api/v1/namespaces/{ns}/pods/{pod_name}/status` | `resources::update_pod_status` |
| `GET` | `/api/v1/namespaces/{ns}/pods/{pod_name}/logs` | `resources::pod_logs` |
| `GET` | `/api/v1/namespaces/{ns}/pods/{pod_name}/exec` | `exec::exec_into_pod` (WebSocket) |

**Workloads**

| Method | Path | Handler |
|--------|------|--------|
| `POST`/`GET` | `/api/v1/namespaces/{ns}/deployments` | `create_deployment` / `list_deployments` |
| `GET`/`PUT` | `/api/v1/namespaces/{ns}/deployments/{name}` | `get_deployment` / `update_deployment` |
| `POST`/`GET` | `/api/v1/namespaces/{ns}/replicasets` | `create_replicaset` / `list_replicasets` |
| `POST`/`GET` | `/api/v1/namespaces/{ns}/daemonsets` | `create_daemonset` / `list_daemonsets` |
| `POST`/`GET` | `/api/v1/namespaces/{ns}/jobs` | `create_job` / `list_jobs` |
| `POST`/`GET` | `/api/v1/namespaces/{ns}/cronjobs` | `create_cronjob` / `list_cronjobs` |
| `POST`/`GET` | `/api/v1/namespaces/{ns}/hpa` | `create_hpa` / `list_hpas` |

**Networking**

| Method | Path | Handler |
|--------|------|--------|
| `POST`/`GET` | `/api/v1/namespaces/{ns}/services` | `create_service` / `list_services` |
| `POST`/`GET` | `/api/v1/namespaces/{ns}/endpoints` | `create_endpoint` / `list_endpoints` |
| `POST`/`GET` | `/api/v1/namespaces/{ns}/ingresses` | `create_ingress` / `list_ingresses` |
| `POST`/`GET` | `/api/v1/namespaces/{ns}/networkpolicies` | `create_network_policy` / `list_network_policies` |

**Configuration & Storage**

| Method | Path | Handler |
|--------|------|--------|
| `POST`/`GET` | `/api/v1/namespaces/{ns}/configmaps` | `create_configmap` / `list_configmaps` |
| `POST`/`GET` | `/api/v1/namespaces/{ns}/secrets` | `create_secret` / `list_secrets` |
| `POST`/`GET` | `/api/v1/namespaces/{ns}/resourcequotas` | `create_resource_quota` / `list_resource_quotas` |
| `POST`/`GET` | `/api/v1/namespaces/{ns}/pvcs` | `create_pvc` / `list_pvcs` |

**Images & Runtime**

| Method | Path | Handler |
|--------|------|--------|
| `GET` | `/api/v1/images` | `images::list_images` |
| `POST` | `/api/v1/images/pull` | `images::pull_image` |
| `DELETE` | `/api/v1/images/{image_id}` | `images::delete_image` |
| `GET` | `/api/v1/runtime` | `runtime::get_runtime_info` |
| `PUT` | `/api/v1/runtime/upgrade` | `runtime::upgrade_runtime` |

**Events & Watch**

| Method | Path | Handler |
|--------|------|--------|
| `GET` | `/api/v1/watch?prefix=...&seq=...` | `watch::watch_events` (SSE) |
| `GET` | `/api/v1/events` | `watch::list_events` |

**System**

| Method | Path | Handler |
|--------|------|--------|
| `GET` | `/api/v1/processes` | `processes::list_processes` |
| `DELETE` | `/api/v1/{resource_type}/{ns}/{name}` | `resources::delete_resource` (generic) |

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

## Fail-Static Guarantees

The system is designed to be **fail-static**: running workloads **must** continue executing even when control plane or agent processes crash or restart. No component failure may cause previously-healthy containers to stop.

### Server Restart Resilience

Restarting `k3rs-server` has **zero impact** on running Pods:

| Component | During Server Downtime | After Server Restart |
|---|---|---|
| **Running Containers** | ✅ Continue running on Agent nodes | ✅ No restart needed |
| **Service Proxy (Pingora)** | ✅ Continues routing (stale in-memory routes from `AgentStateCache`) | ✅ Reconnects → refreshes routes from server |
| **DNS Server** | ✅ Continues resolving (stale in-memory records from `AgentStateCache`) | ✅ Reconnects → refreshes DNS from server |
| **Agent Container Runtime** | ✅ Fully independent | ✅ No state change |
| **Scheduling** | ❌ Paused (no new pod placement) | ✅ Resumes immediately |
| **Controllers** | ❌ Paused (no reconciliation) | ✅ Catch up via level-triggered reconcile |
| **API** | ❌ Unavailable | ✅ Available immediately |

**Agent behavior**: Agents detect Server disconnection and retry with **exponential backoff** (1s → 2s → 4s → 8s → capped at 30s). Existing workloads are unaffected during the entire retry window.

### Agent Crash Resilience

Restarting or crashing `k3rs-agent` **must not** terminate running containers:

**Container Process Independence** (MANDATORY):

- Container processes **must not** be children of the Agent process tree. If the Agent is killed (`SIGKILL`), container processes must continue running.
- **OCI Runtime**: Containers spawned via `youki`/`crun` are already independent — the OCI runtime `create` + `start` detaches the container process from the caller.
- **MicroVM (Virtualization.framework / Firecracker)**: VM processes must be **double-forked** or launched via a helper so they are not reaped when the Agent exits.
- **Invariant**: `kill -9 <agent-pid>` must **never** cause any container to stop.

**Data Plane Continuity** (during Agent downtime):

| Component | During Agent Crash | After Agent Restart |
|---|---|---|
| **Running Containers** | ✅ Continue running (independent processes) | ✅ Reconciled by pod sync loop |
| **Service Proxy (Pingora)** | ❌ Stops (runs in-process) | ✅ Restarts from SlateDB (`/agent/routes`) — serving within milliseconds, before server reconnect |
| **DNS Server** | ❌ Stops (runs in-process) | ✅ Restarts from SlateDB (`/agent/dns-records`) — resolving within milliseconds, before server reconnect |
| **Pod Networking** | ✅ Existing connections continue | ✅ IP allocations restored from state |
| **Heartbeat** | ❌ Stops → Server marks node NotReady/Unknown | ✅ Resumes, node transitions back to Ready |

> **Trade-off**: Service Proxy and DNS run in-process with the Agent for simplicity. During an Agent crash, new service discovery and load balancing are temporarily unavailable, but existing TCP connections to pods continue working because containers and their network stacks are independent. After restart, the `AgentStateCache` restores routing and DNS within milliseconds — independent of server availability.

### Agent Recovery Procedure

When the Agent restarts after a crash, it **must** perform the following recovery steps in order:

1. **Discover Running Containers**
   - Query the OCI runtime for all containers in running state: `<runtime> list --format json`
   - For MicroVMs, scan for running VM processes (PID files or process list)
   - Build an in-memory map of `container_id → pod` from runtime state

2. **Reconcile with Server State**
   - Fetch desired pod list from Server API: `GET /api/v1/pods?fieldSelector=spec.nodeName=<self>`
   - Compare actual (discovered) vs desired (Server) state
   - **Running and desired**: Adopt — update internal tracking, resume health monitoring
   - **Running but NOT desired**: Stop — container was deleted while Agent was down
   - **Desired but NOT running**: Create — container crashed independently while Agent was down

3. **Restore Networking**
   - Rebuild IP allocation table from discovered containers
   - Restart Service Proxy with current service/endpoint state from Server
   - Restart DNS server with current service records

4. **Resume Normal Operation**
   - Resume heartbeat loop
   - Resume pod sync loop (periodic reconciliation)
   - Resume route sync loop (service proxy updates)

**Idempotency**: Every step must be **idempotent** — the same recovery procedure runs regardless of whether the Agent crashed, was gracefully restarted, or is starting for the first time.

### Agent Local State Cache

**Goal**: The Agent must remain operational — pods running, Service Proxy routing, DNS resolving — even when the API Server is unreachable indefinitely.

**Design**: Every successful sync from the API Server writes the full relevant state to disk atomically. On startup or server failure, the Agent loads this cached state immediately and serves it as-is. Server state always wins on reconnect — no merging.

#### State Data Model

```rust
/// In-memory representation of Agent state. Loaded from AgentStore on startup,
/// written back after every successful server sync.
struct AgentStateCache {
    /// Node name (from registration)
    node_name: String,
    /// Assigned node UUID from server registration
    node_id: Option<String>,
    /// Port assigned by server for Agent API (exec/logs)
    agent_api_port: Option<u16>,
    /// Monotonic sequence from server EventLog — used to detect stale/old cache on reconnect
    server_seq: u64,
    /// Timestamp of last successful server sync
    last_synced_at: DateTime<Utc>,
    /// Desired pod specs for this node (from GET /api/v1/nodes/{name}/pods)
    pods: Vec<Pod>,
    /// All Services across all namespaces (for Service Proxy + DNS)
    services: Vec<Service>,
    /// All Endpoints across all namespaces (for Service Proxy backend resolution)
    endpoints: Vec<Endpoint>,
    /// All Ingress rules (for Ingress Proxy)
    ingresses: Vec<Ingress>,
}
```

#### Backing Store: Agent Embedded SlateDB

The Agent stores all persistent state in a **local SlateDB instance** backed by the node's filesystem. This replaces the ad-hoc JSON file approach with a proper embedded KV database — providing ACID transactions, crash-safe writes, and a consistent storage model shared with the Server.

**Why SlateDB on the Agent (not plain JSON files)?**

| Property | JSON files (old) | Agent SlateDB (new) |
|---|---|---|
| **Atomicity** | Custom `write→fsync→rename` per file | Single `WriteBatch` for all keys |
| **Crash safety** | Manual; gaps possible across 3 files | WAL-backed; no partial writes |
| **Tech consistency** | Diverges from Server storage | Same crate, same mental model |
| **Future extensibility** | New state = new file | New state = new key prefix |
| **Concurrent reads** | File locking | MVCC snapshots |

**Backend**: `object_store::local::LocalFileSystem` — no cloud account or network required on edge nodes.

**Storage path**: `<DATA_DIR>/agent/state.db/`

#### Key Schema (Agent SlateDB)

All keys live under the `/agent/` prefix so the database could theoretically be merged with a server-side store in future without collision.

```
/agent/meta          → AgentMeta JSON  { node_id, node_name, agent_api_port, server_seq, last_synced_at }
/agent/pods          → Vec<Pod> JSON array
/agent/services      → Vec<Service> JSON array
/agent/endpoints     → Vec<Endpoint> JSON array
/agent/ingresses     → Vec<Ingress> JSON array
/agent/routes        → HashMap<String,Vec<String>> JSON  (derived: ClusterIP:port → backends)
/agent/dns-records   → HashMap<String,String> JSON       (derived: FQDN → ClusterIP)
```

**Design note**: Each collection is stored as a **single JSON-array value** under a fixed key (not per-object keys). This matches the "always full-overwrite on re-sync" semantics exactly — a `save()` call unconditionally replaces the entire array, so stale entries from removed services/pods automatically disappear. Per-object keys (e.g. `/agent/services/<ns>/<name>`) were considered but rejected because they require explicit deletion of stale keys to avoid accumulation.

`/agent/routes` and `/agent/dns-records` are **derived views** recomputed and written in the **same `WriteBatch`** as the parent collections after every successful sync. No separate file management required.

**Atomic write protocol**: All 7 keys are written in a single `WriteBatch` — either all commit or none do. SlateDB's WAL ensures crash safety without any application-level temp/fsync/rename logic.

#### AgentStore API

```rust
/// Thin wrapper around a local SlateDB instance.
/// Owns all Agent persistent state: identity, pods, services, endpoints, routes, DNS.
pub struct AgentStore {
    db: SlateDb,
}

impl AgentStore {
    /// Open (or create) the SlateDB at `<data_dir>/agent/state.db/`.
    pub async fn open(data_dir: &str) -> Result<Self>;

    /// Save full AgentStateCache atomically (single WriteBatch covering all keys).
    pub async fn save(&self, cache: &AgentStateCache) -> Result<()>;

    /// Load state on startup. Returns None if database is empty (fresh node).
    pub async fn load(&self) -> Result<Option<AgentStateCache>>;

    /// Read only the RoutingTable (for ServiceProxy bootstrap — avoids full load).
    pub async fn load_routes(&self) -> Result<Option<RoutingTable>>;

    /// Read only the DNS records (for DnsServer bootstrap — avoids full load).
    pub async fn load_dns_records(&self) -> Result<Option<HashMap<String, String>>>;
}
```

#### Sync Behavior

**Write (normal connected operation):**
1. Route sync loop fetches services + endpoints from server every 10s
2. Pod sync loop fetches pod list from server every 10s
3. After **each successful fetch** → derive `AgentStateCache` → call `AgentStore::save()` (single `WriteBatch`)
4. `save()` writes: `/agent/meta`, all pod/service/endpoint keys, `/agent/routes`, `/agent/dns-records`

**Read (startup / server unreachable):**
1. On agent startup: call `AgentStore::load()` → hydrate in-memory `AgentStateCache`
2. Call `AgentStore::load_routes()` → pass to `ServiceProxy` → serving within milliseconds (stale-while-revalidate)
3. Call `AgentStore::load_dns_records()` → pass to `DnsServer` → resolving within milliseconds
4. Attempt server connection in background → on success, full re-sync → overwrite all keys with fresh data

#### Agent Connectivity State Machine

```
           ┌──────────────┐
   start   │              │  server responds
  ─────────►  CONNECTING  ├──────────────────────────────────────────┐
           │              │                                          │
           └──────┬───────┘                                 ┌────────▼────────┐
                  │ timeout / error                         │                 │
                  ▼                                         │   CONNECTED     │
           ┌──────────────┐                                 │                 │
           │              │  cache exists on disk           └────────┬────────┘
           │   OFFLINE    │◄──────────────────────                  │ heartbeat/sync fails
           │  (stale ok)  │                                          ▼
           └──────┬───────┘                                 ┌────────────────┐
                  │ background retry → server responds      │                │
                  └─────────────────────────────────────────►  RECONNECTING  │
                                                            │  (exponential  │
                                                            │   backoff)     │
                                                            └────────┬───────┘
                                                                     │ server responds
                                                                     └──► CONNECTED
```

| State | Description |
|---|---|
| **CONNECTING** | Initial startup. If cache exists, start proxy/DNS with stale data while connecting. |
| **CONNECTED** | Server reachable. All syncs succeed. Cache written to disk after every sync. |
| **RECONNECTING** | Heartbeat/sync failing. Agent continues serving stale in-memory state. Retries with exponential backoff (1s → 2s → 4s → 8s → 30s cap). |
| **OFFLINE** | Server unreachable at startup AND cache exists. Load cache → serve stale → keep retrying in background. If no cache: start with empty state, keep retrying. |

#### Behavior by Connectivity State

| Behavior | CONNECTED | RECONNECTING | OFFLINE |
|---|---|---|---|
| **Running pods** | ✅ Server-driven sync | ✅ Keep running (independent processes) | ✅ Keep running (independent processes) |
| **New pod scheduling** | ✅ Normal | ❌ Skipped (server unreachable) | ❌ Skipped |
| **Service Proxy routes** | ✅ Live from server | ✅ Last cached in-memory routes | ✅ Loaded from SlateDB (`/agent/routes`) on startup |
| **DNS resolution** | ✅ Live from server | ✅ Last cached in-memory records | ✅ Loaded from SlateDB (`/agent/dns-records`) on startup |
| **Heartbeat** | ✅ Every 10s | ❌ Failing → server marks node `NotReady` | ❌ No heartbeat |
| **Cache writes** | ✅ After every sync | ❌ No writes (server unreachable) | ❌ No writes |
| **Log output** | Normal | `WARN: server unreachable, retrying (attempt N, age: Xs)` | `WARN: starting in offline mode, cache age: Xs` |

#### Design Principles

1. **Write-through, serve-stale**: Cache is written on every successful sync. An arbitrarily old cache is always preferred over no routing or DNS.
2. **Atomic writes only**: All cache updates committed via a single SlateDB `WriteBatch` — WAL-backed crash safety, no application-level temp/fsync/rename logic required.
3. **Server-wins on reconnect**: Full server state overwrites all SlateDB keys — no merging, no conflict resolution needed.
4. **No cache expiry**: Cache does not expire. Offline agent with 24-hour-old routes is better than offline agent with no routes.
5. **Cache is advisory for pod desired state**: In OFFLINE/RECONNECTING mode, the Agent does **not** create new containers from cached pod specs. It only keeps already-running containers alive and adopts them via the OCI recovery procedure. New pod creation resumes only after server reconnect.
6. **Empty cache is valid**: If the Agent SlateDB is empty (fresh node), Agent starts with empty in-memory state and waits for first server sync. Service Proxy and DNS start with no routes and populate on first sync.

### Design Invariants

1. **No container is a child of the Agent** — Agent crash ≠ container crash
2. **No persistent lock files** — Agent restart does not block on stale locks
3. **Level-triggered reconciliation** — Agent does not rely on missed events; it always compares full actual vs desired state
4. **Server is stateless w.r.t. runtime** — Server never holds container runtime handles or references
5. **Idempotent recovery** — Running the recovery procedure on a healthy Agent is a no-op
6. **Stale state over no state** — Agent always prefers cached state to empty state for networking (Service Proxy + DNS)

### Implementation Checklist

#### Container Process Independence
- [x] OCI backend (`youki`/`crun`): `create` + `start` fully detaches container process from Agent PID tree — inherent to OCI runtime spec; verified via `scripts/test-recovery.sh`
- [x] OCI backend: Integration test — `kill -9 <agent-pid>` → verify container still running via `<runtime> state <id>` — implemented as `scripts/test-recovery.sh`
- [x] VirtualizationBackend (macOS): Launch `k3rs-vmm` helper via `setsid()` (`pre_exec`) + `stdin(Stdio::null())` so VM outlives Agent — implemented in `pkg/container/src/virt.rs::boot_vm()`; PID file written to `<DATA_DIR>/vms/<id>.pid` after spawn; `restore_from_pid_files()` rediscovers alive VMs on restart via `kill(pid, 0)` liveness check; stale PID files removed eagerly
- [x] FirecrackerBackend (Linux): `spawn_firecracker()` fully implemented with `setsid()` + `stdin(Stdio::null())` + PID file for process independence — `pkg/container/src/firecracker/mod.rs`; `restore_from_pid_files()` rediscovers alive VMs on restart via `kill(pid, 0)` liveness check; stale PID files removed eagerly
- [x] PID file management: Write container PID to `<DATA_DIR>/logs/<id>/container.pid` via `--pid-file` flag — **note: path differs from spec** (`runtime/containers/<id>/pid`); used by `read_pid()` for nsenter-based exec

#### Agent Recovery
- [x] `discover_running_containers()` — queries both OCI and VM backends; VM backend lazily initialized via `OnceCell`, FC `list()` calls `restore_from_pid_files()` to recover VMs; tracks each container with correct `runtime_name` — `pkg/container/src/runtime.rs`
- [x] `discover_running_vms()` — implemented as `restore_from_pid_files()` in `pkg/container/src/virt.rs`; scans `<DATA_DIR>/vms/*.pid`, verifies each PID is alive via `kill(pid, 0)`, rebuilds `VmInstance` map; stale PID files removed on startup; wired into `list()` as primary discovery path with `k3rs-vmm ls` as fallback
- [x] `reconcile_with_server(discovered, desired)` — adopt/stop/create logic — implemented inline in agent boot sequence (`cmd/k3rs-agent/src/main.rs`); fetches desired pods from `GET /api/v1/pods?fieldSelector=spec.nodeName=<self>` (Kubernetes-standard endpoint), adopts or stops accordingly
- [x] `restore_ip_allocations(discovered_containers)` — k3rs VMs use virtio-net NAT with DHCP (macOS `Virtualization.framework`); no static IP allocation exists; mapped to `restore_from_pid_files()` which rebuilds the in-memory `VmInstance` `HashMap` — sufficient for VM lifecycle management without a separate IP table
- [x] Refactor Agent boot sequence: use recovery procedure as the **default startup path** (idempotent — works for fresh start and crash recovery) — implemented; recovery runs unconditionally on every agent startup
- [x] Add `GET /api/v1/pods?fieldSelector=spec.nodeName=<name>` endpoint on Server for node-scoped pod queries — implemented in `pkg/api/src/handlers/resources.rs::list_all_pods()`; registered as `GET /api/v1/pods` in `pkg/api/src/server.rs`; also added `fieldSelector` support to namespace-scoped `GET /api/v1/namespaces/{ns}/pods`; agent pod-sync and recovery both updated to use the new standard URL

#### Server Resilience
- [x] Remove `ContainerRuntime` from Server — Server is pure Control Plane
- [x] Remove server lock file system (lock file write/cleanup, colocation guard)
- [x] Update dev scripts (`dev.sh`, `dev-agent.sh`) — remove colocation flags
- [x] Agent exponential backoff on Server disconnect (1s → 2s → 4s → 8s → 16s → 30s cap) — `ConnectivityManager::backoff_duration()` (`cmd/k3rs-agent/src/connectivity.rs`); two bugs fixed:
  - **Off-by-one in heartbeat loop**: `fail_count` is 1-based after first failure; fixed by passing `fail_count.saturating_sub(1)` to convert to 0-based index → first retry now fires after 1s (was 2s)
  - **Shift overflow panic**: `1u64 << attempt` panics when `attempt ≥ 64` (reconnect loop increments unboundedly); fixed by capping shift index at 30 (`attempt.min(30)`) before the left-shift
- [x] Agent: continue running containers when Server unreachable — containers are independent OS processes; pod sync loop uses `continue` on server error, leaving running containers untouched

#### Agent Local State Cache

The following items describe the **initial JSON-file implementation** (completed). The SlateDB migration section below supersedes the storage layer items.

- [x] Define `AgentStateCache` struct — `node_name`, `node_id`, `agent_api_port`, `server_seq`, `last_synced_at`, `pods`, `services`, `endpoints`, `ingresses`; serde Serialize/Deserialize — `cmd/k3rs-agent/src/cache.rs`
- [x] `AgentStateCache::save(path)` — atomic write: serialize to JSON → write to `state.json.tmp` → `fsync` → `rename` to `state.json` — `cache.rs::save()` *(superseded by SlateDB)*
- [x] `AgentStateCache::load(path)` — deserialize from `state.json`; return `None` if file missing (fresh node) — `cache.rs::load()` *(superseded by SlateDB)*
- [x] `AgentStateCache::derive_routes()` → `RoutingTable` (ClusterIP:port → backends); write to `routes.json` — `cache.rs::derive_routes()` *(superseded by SlateDB)*
- [x] `AgentStateCache::derive_dns()` → `HashMap<String, IpAddr>` (FQDN → ClusterIP); write to `dns-records.json` — `cache.rs::derive_dns()` *(superseded by SlateDB)*
- [x] Route sync loop: call `AgentStateCache::save()` + `derive_routes()` + `derive_dns()` after **every** successful server sync — `main.rs` route sync loop
- [x] Pod sync loop: include fetched pods in `AgentStateCache` and save after every successful fetch — `main.rs` pod sync loop
- [x] Agent startup: load cached state before connecting to server → initialize `ServiceProxy` and `DnsServer` with cached data — `main.rs` Phase A + B
- [x] `ServiceProxy::load_from_file(routes_path)` — load `routes.json` on startup for zero-delay route serving — `pkg/proxy/src/service_proxy.rs` *(to be replaced by `load_from_db()`)*
- [x] `DnsServer::load_from_file(dns_path)` — load `dns-records.json` on startup for zero-delay DNS serving — `pkg/network/src/dns.rs` *(to be replaced by `load_from_db()`)*
- [x] Connectivity state machine: `CONNECTING → CONNECTED → RECONNECTING → CONNECTED` (log state transitions) — `cmd/k3rs-agent/src/connectivity.rs`
- [x] `RECONNECTING` state: continue serving stale in-memory state; log `WARN` with cache age on every retry attempt — heartbeat loop in `main.rs`
- [x] `OFFLINE` state: server unreachable at startup; log `WARN: starting in offline mode, cache age: Xs`; keep retrying in background — `main.rs` Phase C
- [x] On reconnect: perform full re-sync from server → overwrite in-memory state and cache (server-wins, no merging) — sync loops resume on `is_connected()`, heartbeat sets `CONNECTED` on recovery
- [x] Agent startup sequence: `load_cache` → `start_services_with_stale` → `connect_server` → `full_sync` → `overwrite_cache` — `main.rs` Phases A→B→C→D→E

#### Agent State Store Migration (JSON → SlateDB)

Replace the ad-hoc JSON file approach with an embedded SlateDB instance.

- [x] Add `slatedb` dependency to `cmd/k3rs-agent/Cargo.toml` (uses workspace version; `object_store` with `LocalFileSystem` is bundled with SlateDB)
- [x] Create `cmd/k3rs-agent/src/store.rs` — implement `AgentStore` struct wrapping a local SlateDB instance
  - [x] `AgentStore::open(data_dir)` — open/create SlateDB at `<DATA_DIR>/agent/state.db/` using `LocalFileSystem` backend
  - [x] `AgentStore::save(cache)` — single `WriteBatch`: write `/agent/meta`, all pod/service/endpoint/ingress keys, `/agent/routes`, `/agent/dns-records`
  - [x] `AgentStore::load()` → `Option<AgentStateCache>` — reads `/agent/meta` first (fast fresh-node check), then `scan_prefix` for each collection; returns `None` if `/agent/meta` missing (fresh node)
  - [x] `AgentStore::load_routes()` → `Option<HashMap<String,Vec<String>>>` — read only `/agent/routes` for fast ServiceProxy bootstrap (future use)
  - [x] `AgentStore::load_dns_records()` → `Option<HashMap<String,String>>` — read only `/agent/dns-records` for fast DnsServer bootstrap (future use)
  - [x] `AgentStore::close()` — flush WAL gracefully on shutdown; called in `main.rs` Ctrl-C handler
- [x] Migrate `AgentStateCache::save()` / `load()` — removed; persistence now via `AgentStore::save()` / `AgentStore::load()`
- [x] Remove custom `atomic_write()` helper from `cache.rs` — replaced by SlateDB `WriteBatch`
- [x] Migrate `AgentStateCache::derive_routes()` / `derive_dns()` — replaced by `derive_routes_map()` / `derive_dns_map()` (pure computation, no file I/O); called inside `AgentStore::save()` to populate `/agent/routes` + `/agent/dns-records` in the same `WriteBatch`
- [x] Bootstrap proxy/DNS without `load_from_file()` — Phase B now calls `service_proxy.update_routes(&cache.services, &cache.endpoints)` and `dns_server.update_records(&cache.services)` directly from the loaded `AgentStateCache`; no separate file load needed
- [x] Update `main.rs` Phase A: `AgentStore::open()` + `store.load()` (replaces `AgentStateCache::load()` from JSON)
- [x] Update `main.rs` Phase B: bootstrap ServiceProxy + DnsServer with cached services/endpoints via `update_routes()` / `update_records()` (replaces `load_from_file()`)
- [x] Update `main.rs` Phase C (registration): update in-memory cache under lock → clone snapshot → `store.save(&snapshot).await` outside lock
- [x] Update `main.rs` reconnect loop: same pattern — update lock → clone → `store.save()`
- [x] Update `main.rs` pod sync loop: same pattern — update lock → clone → `store.save()`
- [x] Update `main.rs` route sync loop: same pattern — update lock → clone → `store.save()` (replaces `c.save()` + `derive_routes()` + `derive_dns()`)
- [x] Remove `cache::routes_path()`, `cache::dns_path()`, `cache::state_path()` path helpers — removed from `cache.rs`
- [ ] Remove `routes.json`, `state.json`, `dns-records.json` file cleanup from `scripts/dev-agent.sh` (no longer written; cleanup is harmless but should be removed for clarity)

#### Testing & Validation

**Unit tests** (`cmd/k3rs-agent/src/tests.rs`) — 24 tests, all passing (`cargo test -p k3rs-agent`):
- [x] `backoff_sequence_is_correct` — verify 0→1s, 1→2s, 2→4s, 3→8s, 4→16s, 5+→30s
- [x] `backoff_does_not_panic_on_large_attempt` — regression for shift-overflow panic (Group 2 fix)
- [x] `heartbeat_backoff_is_1s_on_first_retry` — regression for off-by-one (Group 2 fix)
- [x] `connectivity_*` — 7 state-machine transition tests (CONNECTING→CONNECTED→RECONNECTING→OFFLINE and back)
- [x] `derive_dns_map_*` — 3 tests: FQDN format, headless skipped, empty cache
- [x] `derive_routes_map_*` — 5 tests: single/multi backend, no ClusterIP, no matching endpoints, wrong namespace
- [x] `fresh_store_load_returns_none` — Scenario 4 unit equivalent: empty DB returns None
- [x] `roundtrip_identity_fields` — node_name, node_id, agent_api_port, server_seq survive save→load
- [x] `roundtrip_services_and_endpoints` — collection counts and field values preserved
- [x] `derived_views_are_stored_and_fast_loadable` — Scenario 2 unit: `/agent/routes` + `/agent/dns-records` readable via fast-path helpers
- [x] `second_save_overwrites_first_server_wins` — Scenario 5 unit: full-array overwrite removes stale entries (exposed + fixed stale-key bug in per-object key design)
- [x] `reopen_reads_persisted_data` — data survives `close()` + re-`open()` (simulates process restart)

**Integration tests** (`scripts/test-resilience.sh`) — 7 E2E bash scenarios:
- [x] Scenario 1: kill Server → DNS still resolves (stale in-memory cache) → restart Server → agent reconnects + AgentStore refreshed
- [x] Scenario 2: kill Agent → restart with no server → `AgentStore loaded` appears within 5s → DNS resolves from stale cache offline
- [x] Scenario 3: kill Agent + Server simultaneously → restart both → agent reconnects + AgentStore refreshed
- [x] Scenario 4: Fresh start (no prior data dir) → agent registers normally → no "AgentStore loaded" log
- [x] Scenario 5: Agent syncs svc-old → agent goes offline → svc-new created on server → agent restarts → reconnects → both services resolve via DNS
- [x] Scenario 6 (Group 5): `GET /api/v1/pods?fieldSelector=spec.nodeName=<node>` returns only pods assigned to that node; nonexistent node returns `[]`; no-filter returns all pods; namespace-scoped endpoint also honours `fieldSelector`
- [ ] Scenario 7 (Group 6): `POST /api/v1/cluster/backup` returns gzip file; `POST /api/v1/cluster/restore/dry-run` validates it; `POST /api/v1/cluster/restore` wipes + re-imports; all original pods visible after restore

- [x] Test: kill Agent → verify containers still running → restart Agent → verify pod adoption — `scripts/test-recovery.sh`

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

## Backup & Restore

### Overview

k3rs provides snapshot-based backup and restore of all cluster state. Since all state lives in SlateDB as key-value pairs, a backup is a **full logical dump** of every key-value pair plus PKI certificates — exported as a single portable file.

> **Design Principle**: Backup captures **logical state** (JSON key-value pairs), not physical storage files. This makes backups portable across storage backends (local → S3, S3 → R2) and SlateDB versions.

### Backup Scope

| Data | Included | Source |
|---|---|---|
| Nodes | ✅ | `/registry/nodes/*` |
| Namespaces | ✅ | `/registry/namespaces/*` |
| Pods | ✅ | `/registry/pods/*` |
| Deployments | ✅ | `/registry/deployments/*` |
| ReplicaSets | ✅ | `/registry/replicasets/*` |
| Services | ✅ | `/registry/services/*` |
| Endpoints | ✅ | `/registry/endpoints/*` |
| ConfigMaps | ✅ | `/registry/configmaps/*` |
| Secrets | ✅ (encrypted) | `/registry/secrets/*` |
| RBAC (Roles/Bindings) | ✅ | `/registry/rbac/*` |
| Ingresses | ✅ | `/registry/ingresses/*` |
| NetworkPolicies | ✅ | `/registry/networkpolicies/*` |
| ResourceQuotas | ✅ | `/registry/resourcequotas/*` |
| PVCs | ✅ | `/registry/pvcs/*` |
| HPAs | ✅ | `/registry/hpa/*` |
| DaemonSets | ✅ | `/registry/daemonsets/*` |
| Jobs / CronJobs | ✅ | `/registry/jobs/*`, `/registry/cronjobs/*` |
| Leader Leases | ❌ (ephemeral) | `/registry/leases/*` |
| Events | ❌ (ephemeral) | `/events/*` |
| PKI (CA cert + key) | ✅ | In-memory `ClusterCA` → exported to backup |

### Backup File Format

Backups are stored as **gzip-compressed JSON** (`.k3rs-backup.json.gz`):

```json
{
  "version": 1,
  "created_at": "2026-02-27T15:50:00Z",
  "cluster_id": "abc-123",
  "server_version": "0.1.0",
  "key_count": 142,
  "pki": {
    "ca_cert_pem": "-----BEGIN CERTIFICATE-----\n...",
    "ca_key_pem": "-----BEGIN PRIVATE KEY-----\n..."
  },
  "entries": [
    {
      "key": "/registry/namespaces/default",
      "value": { "name": "default", "id": "...", "created_at": "..." }
    },
    {
      "key": "/registry/pods/default/nginx-abc",
      "value": { "name": "nginx-abc", "namespace": "default", "spec": { ... } }
    }
  ]
}
```

**Why JSON (not binary)?**
- Human-readable → easy to inspect and debug
- Portable → no dependency on SlateDB internals or SST file format
- Diffable → can compare two backups with standard tools
- Filterable → can selectively restore by parsing entries

### Backup Triggers

#### Manual Backup (API + CLI)

```bash
# Create a backup
k3rsctl backup create --output ./cluster-backup.k3rs-backup.json.gz

# Create with custom name
k3rsctl backup create --name "pre-upgrade" --output /backups/

# List existing backups (from backup directory)
k3rsctl backup list --dir /backups/

# Backup info (key count, size, created time)
k3rsctl backup inspect ./cluster-backup.k3rs-backup.json.gz
```

**Server API:**
```
POST /api/v1/cluster/backup          → returns backup as streaming download
GET  /api/v1/cluster/backup/status   → returns last backup info (time, key_count, size)
```

#### Scheduled Backup

Configured via server config or CLI flags:

```yaml
# config.yaml
backup:
  enabled: true
  schedule: "0 */6 * * *"        # every 6 hours (cron format)
  dir: /var/lib/k3rs/backups     # local backup directory
  retention: 7                    # keep last 7 backups, delete older
```

```bash
k3rs-server --backup-dir /var/lib/k3rs/backups --backup-interval 6h --backup-retention 7
```

**Backup Controller** (runs on leader only):
- Interval-based trigger (no full cron parser needed — just hour interval)
- Writes backup to `--backup-dir` with timestamp filename: `backup-20260227-155000.k3rs-backup.json.gz`
- Rotates old backups: keeps `--backup-retention` most recent, deletes the rest
- Emits cluster event on success/failure

### Restore Procedure

Restore replaces **all** cluster state with the backup contents via a running Server — **no server stop required**.

#### How It Works

```
k3rsctl restore --from ./backup.gz
   │
   ▼
POST /api/v1/cluster/restore  (multipart upload)
   │
   ▼ (Leader Server)
1. Validate backup (version, format)
2. Pause all controllers + scheduler
3. Wipe all /registry/ keys
4. Import all entries from backup
5. Reload PKI (CA cert + key)
6. Resume controllers + scheduler
7. Emit restore-complete event
   │
   ▼
Controllers reconcile from restored state
Agents detect state change via watch/heartbeat
```

#### CLI

```bash
# Restore (sends backup to Server via API)
k3rsctl restore --from ./cluster-backup.k3rs-backup.json.gz

# Dry-run (show what would be restored without writing)
k3rsctl restore --from ./cluster-backup.k3rs-backup.json.gz --dry-run

# Force (skip confirmation prompt)
k3rsctl restore --from ./cluster-backup.k3rs-backup.json.gz --force
```

#### Server API

```
POST /api/v1/cluster/restore           → multipart upload backup file, returns restore result
POST /api/v1/cluster/restore/dry-run   → validate + show diff without applying
```

#### Multi-Server Mode

When multiple Servers share SlateDB (object storage):

```
                        ┌─────────────┐
k3rsctl restore ──────► │  Leader     │  ← restore runs here only
                        │  Server     │
                        └──────┬──────┘
                               │ wipe + import to SlateDB
                               │ (shared object storage)
                               ▼
                ┌──────────────────────────────┐
                │     SlateDB (Object Storage) │
                └──────────────────────────────┘
                    ▲              ▲
                    │              │
            ┌───────┴──┐   ┌──────┴───┐
            │ Follower │   │ Follower │  ← detect + reload
            │ Server A │   │ Server B │
            └──────────┘   └──────────┘
```

| Concern | Handling |
|---|---|
| **Who performs restore?** | Leader only — followers reject restore requests with `409 Conflict` |
| **How do followers detect it?** | Leader writes `/registry/_restore/epoch` key (monotonic counter) — followers watch this key |
| **What do followers do?** | On epoch change → pause controllers → reload state from SlateDB → resume |
| **During restore?** | Leader responds to API requests with `503 Service Unavailable` (~1-5 seconds) |
| **Agent impact?** | None — containers keep running (fail-static), agent reconnects + reconciles after restore |

#### Restore Flow (Leader, Internal)

1. **Validate** — parse backup, check `version`, verify key format
2. **Set restore mode** — write `/registry/_restore/status = "in_progress"`, reject new writes
3. **Pause controllers** — stop all 8 controllers + scheduler
4. **Wipe** — scan + delete all `/registry/` keys (except `_restore/`)
5. **Import** — batch write all entries from backup to SlateDB
6. **Reload PKI** — update in-memory `ClusterCA` with restored cert + key
7. **Bump epoch** — write `/registry/_restore/epoch` → triggers follower reload
8. **Resume** — restart controllers + scheduler, clear restore mode, emit event

#### Post-Restore Behavior

| Component | Behavior |
|---|---|
| **Nodes** | Restored as `Unknown` → transition to `Ready` when Agents reconnect |
| **Pods** | Restored with last-known status → Agent reconciles actual vs desired |
| **Deployments** | Controllers resume reconciliation from restored generation |
| **Services/DNS** | Restored → Agents rebuild routing tables on next sync |
| **Secrets** | Restored (still encrypted at rest) |
| **PKI** | CA cert + key restored → existing Agent certs remain valid |
| **Leader Lease** | Kept (current leader continues) |
| **Running Containers** | ✅ Not affected (fail-static) |

#### Selective Restore (future)

```bash
# Restore specific namespace only
k3rsctl restore --from ./backup.gz --namespace production

# Restore specific resource types only
k3rsctl restore --from ./backup.gz --resources deployments,services,configmaps
```

### Implementation Checklist

#### Backup
- [x] `StateStore::snapshot()` — scan all `/registry/` prefixes, return `Vec<(key, value)>`; excludes `_restore/`, `_backup/`, `leases/` — `pkg/state/src/client.rs::snapshot()`
- [x] `BackupFile` struct — `version`, `created_at`, `cluster_name`, `node_count`, `key_count`, `entries: Vec<BackupEntry>`, `pki: BackupPki`; serde Serialize/Deserialize — `pkg/types/src/backup.rs`
- [x] `create_backup_bytes(state)` → gzip-compressed JSON bytes; `validate_backup(backup)` → check version + non-empty — `pkg/api/src/handlers/backup.rs`
- [x] `validate_backup(backup)` → check version string + non-empty entries — `backup.rs::validate_backup()`
- [x] `POST /api/v1/cluster/backup` API endpoint — snapshot + gzip → stream as `application/gzip` download — `backup.rs::create_backup_handler()`
- [x] `GET /api/v1/cluster/backup/status` API endpoint — returns last backup metadata from `/registry/_backup/last` — `backup.rs::backup_status()`
- [x] `BackupController` — scheduled backup with rotation on leader node — `pkg/controllers/src/backup.rs`
    - [x] Interval-based trigger via `tokio::time::interval` (configurable `--backup-interval-secs`, default 3600s)
    - [x] Write backup to `--backup-dir` with timestamp filename: `backup-YYYYMMDD-HHmmss.k3rs-backup.json.gz`
    - [x] Rotate old backups: keep `--backup-retention` most recent, delete the rest — `BackupController::rotate_backups()`
    - [x] Emit cluster event on success (`/events/backup/success`) / failure (`/events/backup/failure`) via `event_log.emit()`
- [x] `k3rsctl backup create` CLI command — POST to server, save gzip to local file — `k3rsctl`
- [x] `k3rsctl backup list` / `k3rsctl backup inspect` / `k3rsctl backup status` CLI commands
- [x] Server config: `--backup-dir`, `--backup-interval-secs`, `--backup-retention` — `cmd/k3rs-server/src/main.rs` + `ServerConfig`

#### Restore
- [x] `POST /api/v1/cluster/restore` endpoint — upload gzip body, leader-only (returns 403 if not leader) — `backup.rs::restore_cluster_handler()`
- [x] `POST /api/v1/cluster/restore/dry-run` endpoint — parse + validate + return diff info without writing — `backup.rs::restore_dry_run_handler()`
- [x] Restore engine: set `restore_in_progress=true` → wipe `/registry/` → import entries → bump epoch → clear flag — `backup.rs::perform_restore()`
- [x] `/registry/_restore/epoch` key — Unix timestamp bumped after successful restore for follower detection
- [x] `/registry/_restore/status` key — `in_progress` / `completed` / `failed` written throughout restore
- [ ] Follower: watch `_restore/epoch` → pause → reload → resume (single-server setup; follower watch loop deferred to multi-server phase)
- [x] `503 Service Unavailable` during restore window — `restore_guard_middleware` added as route_layer in `server.rs`; checks `AppState::restore_in_progress: Arc<AtomicBool>`
- [x] `k3rsctl restore --from <file>` CLI command (with `--force`, `--dry-run`) — `cmd/k3rsctl/src/main.rs::Commands::Restore`


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
    - Agent stores certs to `/etc/k3rs/certs/<node>/` (node.crt, node.key, ca.crt)
    - Token validation on server side (rejects empty or mismatched tokens)
- [x] Implement basic `k3rsctl` CLI with `cluster info` and `node list`.
    - `k3rsctl cluster info` — `GET /api/v1/cluster/info`, displays endpoint, version, state store, node count
    - `k3rsctl node list` — `GET /api/v1/nodes`, displays formatted table (ID, NAME, STATUS, REGISTERED)
    - `--server` flag for configurable API endpoint
    - Agent: heartbeat loop (10s interval), registration with real certs, tunnel proxy startup
- [x] YAML configuration file support (K3s-style).
    - `--config` / `-c` flag on both `k3rs-server` (default `<CONFIG_DIR>/config.yaml`) and `k3rs-agent` (default `<CONFIG_DIR>/agent-config.yaml`)
    - 3-layer merge: CLI args > YAML config file > hardcoded defaults
    - Server config keys: `port`, `data-dir`, `token`
    - Agent config keys: `server`, `token`, `node-name`, `proxy-port`, `service-proxy-port`, `dns-port`
    - Gracefully skips missing config file (uses defaults)
    - **Path constants** (`pkg/constants/src/paths.rs`): Only 3 base directory constants for easy config and uninstall:
      ```rust
      pub const CONFIG_DIR: &str = "/etc/k3rs";      // Configuration files, TLS certs
      pub const DATA_DIR: &str   = "/var/lib/k3rs";   // Persistent data, state, binaries, runtime
      pub const LOG_DIR: &str    = "/var/logs/k3rs";  // Log files
      ```
    - All sub-paths derived at usage site (e.g. `format!("{}/server", DATA_DIR)`, `format!("{}/certs/{}", CONFIG_DIR, node_name)`)
    - **Uninstall**: `rm -rf /etc/k3rs /var/lib/k3rs /var/logs/k3rs`
    - Example server `config.yaml`:
      ```yaml
      port: 6443
      data-dir: /var/lib/k3rs/server
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
    - All types stored in SlateDB using name-based key schema: `/registry/<type>/<ns>/<name>` (not UUID)
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
- [x] Implement container runtime with pluggable `RuntimeBackend` trait.
    - `ContainerRuntime` with platform-aware detection: Virtualization.framework (macOS) → Firecracker / OCI (Linux)
    - Backends: `VirtualizationBackend` (macOS), `FirecrackerBackend` (Linux microVM), `OciBackend` (youki/crun)
    - API: `pull_image`, `create_container`, `start_container`, `stop_container`, `exec_in_container`, `runtime_info`
    - Image pulling via `oci-client`, rootfs extraction via `tar`+`flate2`
    - macOS: boots lightweight Linux microVM per pod via Virtualization.framework (sub-second boot, virtio devices)
    - Linux (microVM): Firecracker microVM per pod — KVM-based, sub-125ms boot, virtio-net/virtio-blk
    - Linux (OCI): runtime via `youki`/`crun`, auto-download from GitHub Releases via `installer.rs`
    - `PodRuntimeInfo` on each Pod tracks which backend + version is running it
    - Runtime Management API: `GET /api/v1/runtime`, `PUT /api/v1/runtime/upgrade`
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
    - **Networking**: Ingress, Network Policies
    - **Policies**: Resource Quotas
    - **Storage**: Volumes (PVCs)
    - **Cluster**: Processes, Events
- [x] Implement Network Policies page — pod selectors, Ingress/Egress type badges.
- [x] Implement Resource Quotas page — max pods, max CPU (cores), max memory per namespace.
- [x] Implement Volumes (PVC) page — storage class, requested size (GB/MB), phase status badges.
- [x] Implement Process List page — real system processes via `sysinfo` (node name, process name, CPU%, memory, PID).
    - Backend: `GET /api/v1/processes` handler using `sysinfo` crate, sorted by memory descending
    - UI: color-coded CPU (>50% red, >10% amber, else green) and memory (>500MB red, >100MB amber, else cyan)
- [x] Add `get_quotas`, `get_network_policies`, `get_pvcs`, `get_metrics`, `get_processes` server functions.
- [x] Dark mode with Tailwind CSS v4.1.5 + `dioxus-free-icons` (Lucide).
- [x] Dioxus server functions (`#[get]`) — reqwest proxies to k3rs API (server-side only).

### Phase 4: Deployments & Controllers
- [x] Implement Deployment and ReplicaSet controllers with rolling update strategy.
    - `DeploymentController` (10s interval): reconciles Deployments → ReplicaSets with `RollingUpdate` and `Recreate` strategies
    - Template hashing for change detection; creates new RS on spec change, scales down old RS
    - `ReplicaSetController` (10s interval): reconciles ReplicaSets → Pods, creates/deletes to match desired count
    - Integrates with `Scheduler` for pod placement; aggregates ready/available status
    - `Pod` extended with `labels`, `owner_ref`, `restart_count` for ownership tracking
    - `Deployment` extended with `selector` (label matching), `generation`/`observed_generation` (rollout tracking)
    - `ReplicaSet` type: `spec.replicas`, `spec.selector`, `spec.template`, `owner_ref`, `template_hash`
- [x] Implement DaemonSet controller.
    - `DaemonSetController` (15s interval): ensures one Pod per eligible node
    - `node_selector` label matching for targeted scheduling
    - Auto-creates pods on new Ready nodes, removes orphan pods when nodes become ineligible
    - `DaemonSet` type: `spec.template`, `spec.node_selector`, `status.desired/current/ready`
- [x] Implement Job / CronJob controller.
    - `JobController` (10s interval): run-to-completion workloads with `completions`, `parallelism`, `backoff_limit`
    - Tracks `active`, `succeeded`, `failed` pod counts; transitions to `Complete` or `Failed`
    - `CronJobController` (30s interval): spawns Jobs on cron schedule (minute-field MVP parser)
    - Supports `*/N` (every N minutes), `M` (at minute M), `*` (every minute); `suspend` flag
    - `CronJob` type: `spec.schedule`, `spec.job_template`, `spec.suspend`, `status.active_jobs`
- [x] Implement Horizontal Pod Autoscaler (HPA).
    - `HPAController` (30s interval): scales Deployment replicas based on CPU/memory utilization thresholds
    - 10% hysteresis to prevent flapping; respects `min_replicas`/`max_replicas` bounds
    - `HPA` type: `spec.target_deployment`, `spec.min/max_replicas`, `spec.metrics.cpu/memory_utilization_percent`
    - Simulated metrics baseline (70% CPU, 60% memory) — real agent metrics in Phase 6
- [x] Implement `k3rsctl apply`, `k3rsctl logs`, `k3rsctl exec`.
    - `k3rsctl get` extended: `replicasets`/`rs`, `daemonsets`/`ds`, `jobs`, `cronjobs`/`cj`, `hpa`
    - `k3rsctl apply` extended: `ReplicaSet`, `DaemonSet`, `Job`, `CronJob`, `HorizontalPodAutoscaler` kinds
    - `k3rsctl logs <pod>` — fetches `GET /api/v1/namespaces/:ns/pods/:id/logs`
    - `k3rsctl exec <pod> -- <cmd>` — WebSocket client connecting to real container runtime exec
    - `k3rsctl exec <pod>` — interactive mode (stdin loop over WebSocket)
    - `k3rsctl runtime info` — show current container runtime backend + version
    - `k3rsctl runtime upgrade` — trigger auto-download of latest runtime (Linux)
    - API: `GET/PUT /deployments/:id`, `POST/GET` for replicasets/daemonsets/jobs/cronjobs/hpa
    - All 7 controllers (Node + 6 new) started at server boot

### Phase 5: Reliability & High Availability
- [x] Implement multi-server mode with leader election via SlateDB leases.
    - `LeaderElection` engine in `pkg/state/src/leader.rs` using `/registry/leases/controller-leader` key
    - TTL-based lease (15s) with automatic renewal every 5s
    - Leader-gated controllers: only the leader runs Scheduler + all 8 controllers
    - On leadership loss: controllers are aborted; on re-acquisition: controllers restart
    - All servers serve API reads regardless of leader status
- [x] Implement graceful node shutdown and Pingora zero-downtime proxy upgrades.
    - `POST /api/v1/nodes/:name/cordon` — mark node unschedulable + add NoSchedule taint
    - `POST /api/v1/nodes/:name/uncordon` — remove unschedulable flag + taint, restore Ready
    - `POST /api/v1/nodes/:name/drain` — cordon + evict all pods (reset to Pending for rescheduling)
    - `Node.unschedulable` field used by Scheduler to skip cordoned nodes
    - Agent handles SIGTERM: graceful exit
    - `k3rsctl node drain/cordon/uncordon <name>` CLI commands
- [x] Implement workload rescheduling on node failure.
    - `EvictionController` (30s interval) watches for nodes in `Unknown` state
    - After 5-minute grace period, evicts all pods from failed nodes
    - Evicted pods reset to `Pending` with `node_name = None` for automatic rescheduling
    - Skips master/control-plane nodes and already-terminal pods
- [x] Implement namespace resource quotas and network policies.
    - `ResourceQuota` type: `max_pods`, `max_cpu_millis`, `max_memory_bytes` per namespace
    - `POST/GET /api/v1/namespaces/:ns/resourcequotas` CRUD endpoints
    - `NetworkPolicy` type: pod selector, ingress/egress rules, peer/port matching
    - `POST/GET /api/v1/namespaces/:ns/networkpolicies` CRUD endpoints

### Phase 6: Observability & Extensibility
- [x] Add Prometheus-compatible `/metrics` endpoints on Server and Agent.
    - New `pkg/metrics` crate with atomic counters and gauges, Prometheus text exposition format
    - `GET /metrics` on server: `k3rs_api_requests_total`, `k3rs_nodes_total`, `k3rs_pods_total`, `k3rs_leader_status`, `k3rs_controller_reconcile_total`
    - Request ID middleware (`x-request-id` header + tracing span)
- [x] Implement structured JSON logging across all components.
    - `--log-format json` flag for server and agent
    - Uses `tracing-subscriber` JSON layer with `env-filter` support
- [x] Implement container log streaming via `k3rsctl logs`.
    - `container_logs(id, tail)` method on `ContainerRuntime` (stub returns simulated timestamped logs)
    - `k3rsctl logs --follow` flag for polling-based log streaming (2s interval)
- [x] OpenTelemetry tracing integration (future).
    - `--enable-otel` flag on server (stub — logs a message, ready for future collector)
- [x] CSI-based persistent storage interface (future).
    - `Volume`, `VolumeMount`, `VolumeSource` types (HostPath/EmptyDir/PVC/ConfigMap/Secret)
    - `PersistentVolumeClaim` type with storage class, access modes, phase
    - `POST/GET /api/v1/namespaces/:ns/pvcs` CRUD endpoints (auto-bind in stub mode)
    - `volumes` field on `PodSpec`, `volume_mounts` on `ContainerSpec`
- [x] Blue/Green and Canary deployment strategies via Pingora (future).
    - `BlueGreen` variant: deploy new version at full scale, cut over by scaling old to 0
    - `Canary { weight }` variant: deploy canary replicas proportional to traffic percentage
    - Both strategies handled in `DeploymentController`

### Phase 7: Implementation Complete

#### Container Runtime (`pkg/container/`) — Virtualization + OCI 🏆
Platform-aware, daemonless container runtime with pluggable `RuntimeBackend` trait:

**Architecture:** macOS = Virtualization.framework microVM | Linux = Firecracker microVM or `youki`/`crun` OCI runtime

| Module | Crate | Purpose |
|--------|-------|---------|
| `image.rs` | `oci-client` | Pull images from OCI registries (Docker Hub, GHCR) with `linux_platform_resolver` for cross-platform multi-arch resolution |
| `rootfs.rs` | `tar` + `flate2` | Extract image layers → rootfs + generate production OCI `config.json` (capabilities, mounts, rlimits, masked/readonly paths, env passthrough) |
| `backend.rs` | — | `RuntimeBackend` trait + Virtualization/Firecracker/OCI backends + PID tracking + `state()` query |
| `state.rs` | `dashmap` | In-process container state tracking (`ContainerStore`) — lifecycle: Created → Running → Stopped/Failed |
| `virt.rs` | `objc2-virtualization` | macOS Virtualization.framework microVM backend |
| `firecracker/` | `reqwest`, `flate2`, `tar` | Linux Firecracker microVM backend — spawns `firecracker` binary, configures via REST API over Unix socket, ext4 rootfs via `mkfs.ext4 -d`, TAP+NAT networking, vsock exec |
| `runtime.rs` | — | `ContainerRuntime` facade with platform detection, `ContainerStore` integration, `exec_in_container`, `cleanup_container` |
| `installer.rs` | `reqwest` | Auto-download youki/crun/firecracker from GitHub Releases (Linux) |

**Guest Components:**

| Binary | Purpose |
|--------|---------|
| `k3rs-vmm` | Host-side VMM helper — wraps Virtualization.framework via `objc2-virtualization` Rust crate (macOS) |
| `k3rs-init` | Guest-side PID 1 — mounts pseudo-fs, brings up networking via raw `ioctl`, reaps zombies, parses OCI `config.json`, spawns entrypoint, graceful VM shutdown. Statically-linked musl binary |

**Backends:**
- [x] `VirtualizationBackend` — lightweight Linux microVM via Apple Virtualization.framework (macOS)
- [x] `FirecrackerBackend` — Firecracker microVM via KVM (Linux) — sub-125ms boot, virtio devices — `pkg/container/src/firecracker/mod.rs` with full `RuntimeBackend` impl: create, start, stop, delete, list, logs, exec, spawn_exec, state; spawns `firecracker` binary via REST API, ext4 rootfs via `mkfs.ext4 -d` (no root), TAP+NAT networking with kernel `ip=` config, vsock exec via host→guest CONNECT handshake, PID-file recovery, process independence via `setsid()`, VM backend cached in `OnceCell` for cross-call state persistence
- [x] `OciBackend` — invokes `youki`/`crun` via `std::process::Command` (Linux) — complete implementation, no mocking/fallback

**OCI Runtime Features (Complete):**
- [x] Production OCI `config.json` — Docker-compatible Linux capabilities (14 caps), 7 mount points (`/proc`, `/dev`, `/dev/pts`, `/dev/shm`, `/dev/mqueue`, `/sys`, `/sys/fs/cgroup`), `RLIMIT_NOFILE`, masked paths (`/proc/kcore`, `/proc/keys`, etc.), readonly paths (`/proc/bus`, `/proc/sys`, etc.)
- [x] Container state tracking — `ContainerStore` via `DashMap` (concurrent in-process): tracks lifecycle state, PID, exit code, timestamps, log/bundle paths
- [x] PID tracking — `--pid-file` flag on create, `--root` custom state directory
- [x] OCI runtime state query — `state()` method runs `<runtime> state <id>`, parses JSON
- [x] Log directory management — structured log paths at `<DATA_DIR>/runtime/logs/<id>/stdout.log`
- [x] Environment variable passthrough — pod `ContainerSpec.env` → OCI `config.json` `process.env`
- [x] User namespace with 65536 UID/GID range mapping (rootless-compatible)
- [x] Network namespace isolation
- [x] Agent pod sync — proper error handling with `status_message` reporting (`ImagePullError`, `ContainerCreateError`, `ContainerStartError`)
- [x] Pod type — `status_message: Option<String>` + `container_id: Option<String>` fields
- [x] Container cleanup — `cleanup_container()` for failed containers (stop + delete + remove from store + cleanup dir)
- [x] Container spec passthrough — command, args, env from pod spec into OCI container

**VirtualizationBackend (macOS):**
- [x] Apple Virtualization.framework via `objc2-virtualization` Rust crate
- [x] Each pod runs in an isolated lightweight microVM
- [x] VM lifecycle: create → boot → stop → delete
- [x] Container logs via log file (virtio-console ready)
- [x] Exec fallback on host when VMM helper unavailable
- [x] Platform detection: macOS → VirtualizationBackend → OCI fallback
- [x] `linux_platform_resolver` for cross-platform multi-arch OCI image pulling
- [x] **virtio-fs**: mount host rootfs folder directly as guest `/` (no disk image creation — replaces `hdiutil`/`dd`)
  - `VZVirtioFileSystemDeviceConfiguration` + `VZSharedDirectory` → guest mounts via `mount -t virtiofs`
  - Zero overhead: no pre-allocation, no rootfs → block device conversion
- [x] `k3rs-vmm` helper binary — wraps Virtualization.framework via `objc2-virtualization` Rust crate (rewritten from Swift)
- [x] `k3rs-init` — minimal Rust PID 1 for guest VM (`cmd/k3rs-init/`):
  - Mount `/proc`, `/sys`, `/dev`, `/dev/pts`, `/dev/shm`, `/tmp`, `/run` via `libc::mount()`
  - Set hostname via `nix::unistd::sethostname`, bring up `lo`/`eth0` via raw `ioctl(SIOCSIFFLAGS)`
  - Reap zombies via `waitpid(-1, WNOHANG)` + `SIGCHLD → SigIgn` auto-reap
  - Parse OCI `config.json` (`process.args/env/cwd`, `hostname`) → spawn entrypoint as child
  - Graceful shutdown: `SIGTERM → SIGKILL → umount2 → sync → reboot(POWER_OFF)`
  - Static musl binary, `panic="abort"`, `opt-level="z"`, `lto=true`, `strip=true`
  - Cross-compile from macOS: `cargo zigbuild --release --target aarch64-unknown-linux-musl -p k3rs-init`
- [x] virtio-net: NAT networking via `VZNATNetworkDeviceAttachment`
- [x] virtio-console: stream stdout/stderr to host log file via `VZVirtioConsoleDeviceSerialPortConfiguration`
- [x] virtio-vsock: host ↔ guest exec channel via `VZVirtioSocketDeviceConfiguration` (port 5555)
- [x] Bundle minimal Linux kernel (`vmlinux`) + initrd containing `k3rs-init` — `scripts/build-kernel.sh` builds kernel (Linux 6.12) + initrd via Docker/native cross-compile; `pkg/container/src/kernel.rs` (`KernelManager`) handles discovery + optional auto-download
- [ ] Sub-second boot time on Apple Silicon — not measured/verified

**FirecrackerBackend (Linux) — spawns pre-built Firecracker binary, configures via REST API:**

| Module | Purpose |
|--------|---------|
| `firecracker/mod.rs` | `RuntimeBackend` trait impl — full VM lifecycle, vsock exec (host→guest CONNECT handshake), PID-file recovery |
| `firecracker/api.rs` | Lightweight HTTP/1.1 client over Unix socket — proper response parsing (no `shutdown()` race), 30s timeout |
| `firecracker/installer.rs` | KVM detection (`/dev/kvm`), auto-download Firecracker+Jailer from GitHub Releases |
| `firecracker/network.rs` | TAP device creation, /30 subnets, iptables NAT masquerade |
| `firecracker/rootfs.rs` | ext4 image via `mkfs.ext4 -d` (no root required), virtiofsd daemon for other VMMs |
| `firecracker/jailer.rs` | Jailer wrapper — chroot + seccomp + cgroups + daemonize |

- [x] `firecracker/mod.rs` — full `RuntimeBackend` trait implementation (create, start, stop, delete, list, logs, exec, spawn_exec, state)
- [x] KVM detection: `FcInstaller::kvm_available()` checks `/dev/kvm` read+write access — `firecracker/installer.rs`
- [x] Firecracker binary auto-download from GitHub Releases (`firecracker-v{VERSION}-{ARCH}.tgz`) — `firecracker/installer.rs`
- [x] Kernel loading via Firecracker REST API (`/boot-source`) — kernel path from `KernelManager` shared with VZ backend
- [x] `ext4` rootfs: `mkfs.ext4 -d` populates image at format time (no loop mount, no root) — `firecracker/rootfs.rs`
- [x] `virtio-net`: TAP device per VM with /30 subnet + iptables NAT masquerade — `firecracker/network.rs`; guest IP configured via kernel `ip=` boot parameter
- [x] Serial console: `console=ttyS0` in boot args, Firecracker stdout/stderr redirected to log file
- [x] `vsock`: host ↔ guest exec channel via Firecracker vsock — host-initiated `CONNECT {port}\n` handshake on main UDS; one-shot (`exec_via_vsock`) + streaming PTY via `socat` (`spawn_exec`)
- [x] `k3rs-init` as PID 1 inside guest (same binary as macOS backend, cross-compiled via `cargo-zigbuild`)
- [x] Jailer module: chroot + seccomp + cgroups + UID/GID mapping + daemonize — `firecracker/jailer.rs` (available, not wired as default; direct spawn used for development)
- [x] Platform detection: Linux + `/dev/kvm` → FirecrackerBackend (no OCI fallback for `runtime: vm`) — `runtime.rs`
- [x] Process independence: `setsid()` via `pre_exec`, PID file at `{vm_dir}/{id}.pid`, `restore_from_pid_files()` for post-restart recovery
- [x] Guest DNS: `/etc/resolv.conf` injected into rootfs with public DNS (8.8.8.8, 8.8.4.4)
- [x] VM backend instance caching: `OnceCell<Arc<dyn RuntimeBackend>>` in `ContainerRuntime` ensures in-memory VM state persists across create → start → stop → delete calls — `runtime.rs`
- [x] API client: proper HTTP response parsing (headers → Content-Length → body) instead of `shutdown()` + `read_to_end()` which raced with Firecracker's `micro_http` — `firecracker/api.rs`
- [x] Interactive exec routing: `handle_tty()` checks `backend_name_for(id)` to correctly route VM containers through vsock PTY path instead of OCI runtime — `api.rs`
- [x] Agent restart VM recovery: `discover_running_containers()` queries both OCI and VM backends; FC backend's `list()` → `restore_from_pid_files()` recovers running VMs — `runtime.rs`
- [ ] Sub-125ms boot time measurement/verification (boot timer exists in `configure_and_boot()`)
- [x] Support x86_64 and aarch64 (installer auto-detects arch via `std::env::consts::ARCH`)

**Auto-download (Linux):**
- [x] firecracker v1.14.2 from `github.com/firecracker-microvm/firecracker/releases` — `firecracker/installer.rs`
- [x] youki v0.6.0 from `github.com/youki-dev/youki/releases`
- [x] crun 1.26 from `github.com/containers/crun/releases`
- [x] Configurable: `ensure_runtime(Some("crun"))` — default: youki

**Pod Runtime Tracking:**
- [x] `PodRuntimeInfo { backend, version }` on each Pod
- [x] `Pod.status_message` — human-readable error reason for failed containers
- [x] `Pod.container_id` — maps pod to its OCI container ID for runtime queries

#### Image & Registry Management (multi-node)
- [x] `GET /api/v1/images` — aggregated image list across all nodes
- [x] `POST /api/v1/images/pull` — pull image from OCI registry
- [x] `DELETE /api/v1/images/{id}` — delete cached image
- [x] `PUT /api/v1/nodes/{name}/images` — agent reports per-node images (every 30s)
- [x] `ImageInfo` — id, node_name, size, layers, architecture, os
- [x] UI: Images page in sidebar (Cluster section) with per-node table

#### Pod Logs (`pkg/api/src/handlers/resources.rs`)
- [x] Pod logs wired to `ContainerRuntime::container_logs()` via `AppState`

#### CSI Volumes (`pkg/api/src/handlers/resources.rs`)
- [x] PVCs start as `Pending`, background task binds after 2s (`Pending` → `Bound`)

#### OpenTelemetry (`cmd/k3rs-server/src/main.rs`)
- [x] `--enable-otel` initializes OTLP tracing pipeline via `opentelemetry-otlp`
- [x] `--otel-endpoint` flag (default `http://localhost:4317`)

#### CLI — Exec (`cmd/k3rsctl/src/main.rs`)
- [x] WebSocket exec endpoint: `GET /api/v1/namespaces/{ns}/pods/{id}/exec`
- [x] Handler in `pkg/api/src/handlers/exec.rs` — wired to `runtime.exec_in_container()`
- [x] `k3rsctl exec` — WebSocket client via `tokio-tungstenite` (interactive + non-interactive)
- [x] Runtime management: `k3rsctl runtime info`, `k3rsctl runtime upgrade`
- [x] API: `GET /api/v1/runtime`, `PUT /api/v1/runtime/upgrade`

#### Deployment Strategies (`pkg/controllers/src/deployment.rs`)
- [x] BlueGreen — full-scale new RS → scale old to 0 (cutover)
- [x] Canary — weighted replica scaling based on traffic percentage

#### Networking (`pkg/network/src/`)
- [x] CNI — `PodNetwork` IP allocation from CIDR (`cni.rs`, 176 lines + tests)
- [x] DNS — `DnsServer` UDP responder for `svc.cluster.local` resolution (`dns.rs`, 193 lines)

## Project Structure

```text
k3rs/
├── cmd/
│   ├── k3rs-server/            # Control plane binary
│   ├── k3rs-agent/             # Data plane binary
│   ├── k3rs-init/              # Guest PID 1 — minimal init for microVMs (static musl binary)
│   ├── k3rs-vmm/               # Host VMM helper — Virtualization.framework via objc2-virtualization (macOS, Rust)
│   ├── k3rs-ui/                # Management UI (Dioxus web app)
│   └── k3rsctl/                # CLI tool binary
├── pkg/
│   ├── api/                    # Axum HTTP API & handlers
│   ├── constants/              # Centralized constants (paths, network, runtime, auth, state, vm)
│   ├── container/              # Container runtime (Virtualization.framework on macOS, Firecracker/youki/crun on Linux; firecracker/ submodule: mod.rs, api.rs, installer.rs, jailer.rs, network.rs, rootfs.rs)
│   ├── controllers/            # Control loops (Deployment, ReplicaSet, DaemonSet, Job, CronJob, HPA)
│   ├── metrics/                # Prometheus-format metrics registry
│   ├── network/                # CNI (pod networking) & DNS (svc.cluster.local)
│   ├── pki/                    # CA and mTLS certificate management
│   ├── proxy/                  # Pingora-based Service, Ingress & Tunnel proxy
│   ├── scheduler/              # Workload placement logic
│   ├── state/                  # SlateDB storage integration
│   └── types/                  # Cluster object models
└── spec.md                     # Project specification
```

## Tech Stack
- **Language**: Rust
- **HTTP API**: `axum`
- **Management UI**: `dioxus` 0.7 (Rust-native fullstack web framework, WASM SPA)
- **Container Runtime**: Platform-aware with pluggable `RuntimeBackend` trait
  - **macOS**: Virtualization.framework microVM backend via `objc2-virtualization` (lightweight Linux VMs)
  - **Linux (microVM)**: Firecracker binary + REST API over Unix socket (auto-downloaded from GitHub Releases); ext4 rootfs, TAP+NAT networking, vsock exec
  - **Linux (OCI)**: `youki` / `crun` — auto-download from GitHub Releases (fallback when KVM unavailable)
  - **Image Pull**: `oci-client` (OCI Distribution spec — Docker Hub, GHCR, etc.)
  - **Rootfs**: `tar` + `flate2` (extract image layers → host folder), mounted in guest via `virtio-fs`
  - **Guest Init**: `k3rs-init` — static Rust binary as PID 1 (mount `/proc`/`/sys`/`/dev`, reap zombies, `exec()` entrypoint)
  - **WebSocket Exec**: `tokio-tungstenite` for interactive container sessions
  - **VM Comms**: `virtio-fs` for rootfs sharing, `virtio-vsock` for exec, `virtio-console` for logs
- **Storage**: `slatedb` (Embedded key-value database on object storage)
- **Object Storage**: S3 / Cloudflare R2 / MinIO / Local filesystem
- **DNS**: `hickory-dns` (Embedded DNS resolver)
- **Observability**: `opentelemetry` + `opentelemetry-otlp` (OTLP tracing export)
- **Serialization**: `serde`, `serde_json`
- **Async Runtime**: `tokio`
- **CLI**: `clap` (CLI argument parsing)
- **Crypto**: `rustls` (TLS), `rcgen` (Certificate generation)

---

## VPC Networking & Ghost IPv6

> Adapted from Sync7 Ghost IPv6 Architecture (RFC-001, RFC-002).

### Overview

K3rs currently uses a **single flat overlay network** (`10.42.0.0/16`) where every Pod and VM gets a unique IPv4 address. This works for simple deployments but offers no network isolation between workloads.

This specification introduces **VPC (Virtual Private Cloud)** as a first-class resource in K3rs, powered by a **Ghost IPv6** addressing scheme adapted from Sync7. The core idea:

> **IPv4 inside the Pod/VM is a local illusion.**
> All internal routing, isolation, and forwarding decisions operate on **Ghost IPv6**.

A Ghost IPv6 address deterministically encodes `(ClusterID, VpcID, GuestIPv4)`, enabling:

- **Overlapping IPv4 CIDRs** across different VPCs
- **Hard VPC isolation** without Linux network namespaces per VPC
- **Stateless routing** — route decisions derived from packet headers alone
- Pods, Deployments, and VMs bound to a specific VPC

### Design Goals

1. **VPC as a first-class resource** — create, delete, list VPCs via API
2. **Workload binding** — Pods, Deployments, and VMs declare their VPC in spec
3. **Overlapping CIDRs** — different VPCs may use the same IPv4 range (e.g., both use `10.0.0.0/24`)
4. **Hard isolation by default** — Pods in different VPCs cannot communicate unless explicitly peered
5. **Ghost IPv6 as true identity** — the only routable address in the data plane
6. **Fail-static** — existing VPC connectivity survives Agent/Server restarts
7. **Backward compatible** — a `default` VPC preserves current flat-network behavior

### Terminology

| Term | Definition |
|------|-----------|
| **VPC** | A virtual network with its own IPv4 CIDR. Workloads inside a VPC can communicate freely. |
| **Ghost IPv6** | A 128-bit address encoding `(ClusterID, VpcID, GuestIPv4)` — the true routable identity. |
| **ClusterID** | A 32-bit identifier for this K3rs cluster. Fixed at cluster init (stored in SlateDB). |
| **VpcID** | A 16-bit namespace identifier. Unique within a cluster. `0` is reserved. |
| **GuestIPv4** | The IPv4 address visible inside the Pod/VM. Only meaningful within its VPC. |
| **Platform Prefix** | Fixed 32-bit IPv6 prefix. Constant per cluster (ULA range `fc00::/7`). |

### Ghost IPv6 Address Layout (128 bits)

```text
   0                   1                   2                   3
   0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
  +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
  |                     Platform Prefix (32)                      |
  +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
  |  Ver  |      Flags (12)       |      ClusterID (High 16)      |
  +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
  |      ClusterID (Low 16)       |           VpcID (16)          |
  +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
  |                        GuestIPv4 (32)                         |
  +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

| Field | Size | Description |
|-------|------|-------------|
| **Platform Prefix** | 32 bits | Fixed ULA prefix per cluster (e.g., `fd00:k3rs::/32`). Constant. |
| **Ver** | 4 bits | Layout version. MUST be `1`. |
| **Flags** | 12 bits | Reserved. MUST be `0` for v1. |
| **ClusterID** | 32 bits | Unique cluster identifier. Fixed at `k3rs-server` init. Enables future multi-cluster peering. |
| **VpcID** | 16 bits | VPC identifier. `0` is reserved. Up to 65,535 VPCs per cluster. |
| **GuestIPv4** | 32 bits | Pod/VM IPv4 address (only meaningful inside its VPC). |

**Construction** (Rust pseudocode):

```rust
fn construct_ghost_ipv6(
    platform_prefix: u32,
    cluster_id: u32,
    vpc_id: u16,
    guest_ipv4: Ipv4Addr,
) -> Ipv6Addr {
    let mut b = [0u8; 16];
    b[0..4].copy_from_slice(&platform_prefix.to_be_bytes());
    b[4] = 0x10; // ver=1, flags_high=0
    b[5] = 0x00; // flags_low=0
    b[6..8].copy_from_slice(&((cluster_id >> 16) as u16).to_be_bytes());
    b[8..10].copy_from_slice(&((cluster_id & 0xFFFF) as u16).to_be_bytes());
    b[10..12].copy_from_slice(&vpc_id.to_be_bytes());
    b[12..16].copy_from_slice(&guest_ipv4.octets());
    Ipv6Addr::from(b)
}
```

**Example**:
```
Cluster Prefix: fd00:0001::/32
ClusterID: 1
VpcID: 5
GuestIPv4: 10.0.1.10

Ghost IPv6: fd00:0001:1000:0000:0001:0005:0a00:010a
```

**Why ClusterID instead of TenantID?**

Sync7 is a multi-tenant cloud — TenantID isolates customers. K3rs is a single-cluster orchestrator — there are no "tenants" in the cloud sense. ClusterID serves two purposes:
1. Identifies this cluster's traffic in the Ghost IPv6 address space
2. Enables future multi-cluster federation/peering (two K3rs clusters with different ClusterIDs can have overlapping VpcIDs without collision)

### VPC Resource Model

#### VPC Definition

```rust
struct Vpc {
    name: String,           // e.g., "production", "staging", "dev"
    vpc_id: u16,            // Auto-allocated by control plane (1..65535)
    ipv4_cidr: String,      // e.g., "10.0.0.0/16"
    status: VpcStatus,      // Active, Terminating, Deleted
    created_at: DateTime<Utc>,
}

enum VpcStatus {
    Active,
    Terminating,  // Draining: no new workloads, existing continue
    Deleted,
}
```

#### SlateDB Key Prefix

```
/registry/vpcs/<vpc-name>                → VPC definition & status
```

#### VPC API

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/v1/vpcs` | Create VPC |
| `GET` | `/api/v1/vpcs` | List all VPCs |
| `GET` | `/api/v1/vpcs/{name}` | Get VPC details |
| `DELETE` | `/api/v1/vpcs/{name}` | Delete VPC (initiates drain) |
| `GET` | `/api/v1/vpcs/{name}/pods` | List pods in VPC |

#### Default VPC

On cluster init, a `default` VPC is automatically created:

```
name: "default"
vpc_id: 1
ipv4_cidr: "10.42.0.0/16"   # Matches current K3rs default
```

Workloads that don't specify a VPC are placed in `default`. This preserves backward compatibility with existing manifests.

### Workload Binding

#### PodSpec Changes

```rust
struct PodSpec {
    pub containers: Vec<ContainerSpec>,
    pub runtime: Option<String>,
    pub vpc: Option<String>,        // NEW: VPC name (default: "default")
    pub node_affinity: HashMap<String, String>,
    pub tolerations: Vec<Toleration>,
    pub volumes: Vec<Volume>,
}
```

#### DeploymentSpec Changes

```rust
struct DeploymentSpec {
    pub replicas: u32,
    pub template: PodSpec,          // PodSpec already has `vpc` field
    pub strategy: DeploymentStrategy,
    pub selector: HashMap<String, String>,
}
```

The `vpc` field propagates from DeploymentSpec → ReplicaSet → Pod automatically via the template.

#### Pod Network Info

When a Pod is scheduled and its container created, the Agent records:

```rust
struct Pod {
    // ... existing fields ...
    pub ghost_ipv6: Option<String>,     // NEW: Ghost IPv6 address assigned
    pub vpc_name: Option<String>,       // NEW: Resolved VPC name
}
```

#### Manifest Examples

**Pod in a specific VPC:**
```json
{
  "name": "web-server",
  "namespace": "production",
  "spec": {
    "vpc": "frontend-vpc",
    "containers": [
      { "name": "nginx", "image": "nginx:latest" }
    ]
  }
}
```

**Deployment (VPC propagated via template):**
```json
{
  "name": "api-service",
  "namespace": "production",
  "spec": {
    "replicas": 3,
    "template": {
      "vpc": "backend-vpc",
      "containers": [
        { "name": "api", "image": "myapp:v2" }
      ]
    }
  }
}
```

**No VPC specified (uses default):**
```json
{
  "name": "legacy-app",
  "namespace": "default",
  "spec": {
    "containers": [
      { "name": "app", "image": "legacy:v1" }
    ]
  }
}
```

### Network Isolation Model

#### Isolation Rules

| Source → Destination | Behavior |
|---------------------|----------|
| Same VPC | **ALLOWED** — free communication |
| Different VPCs (no peering) | **DENIED** — packets dropped |
| Different VPCs (peered) | **ALLOWED** — explicit peering required |

#### Default Behavior

- **Intra-VPC**: All Pods within the same VPC can reach each other on their GuestIPv4 addresses. No policy needed.
- **Cross-VPC**: Blocked by default. The Service Proxy and CNI enforce isolation by checking VpcID in Ghost IPv6 headers.
- **External traffic**: Ingress and egress to/from the cluster are handled by Ingress Proxy and are VPC-aware (see below).

#### VPC Peering (Optional)

```rust
struct VpcPeering {
    name: String,
    vpc_a: String,          // VPC name
    vpc_b: String,          // VPC name
    direction: PeeringDirection,
    status: PeeringStatus,
    created_at: DateTime<Utc>,
}

enum PeeringDirection {
    Bidirectional,      // Both can initiate
    InitiatorOnly,      // Only vpc_a can initiate to vpc_b
}

enum PeeringStatus {
    Active,
    Inactive,
}
```

**SlateDB Key**: `/registry/vpc-peerings/<peering-name>`

**API**:

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/v1/vpc-peerings` | Create peering |
| `GET` | `/api/v1/vpc-peerings` | List peerings |
| `DELETE` | `/api/v1/vpc-peerings/{name}` | Delete peering |

### CNI Changes (PodNetwork)

The current `PodNetwork` manages a single flat CIDR. With VPCs, each VPC gets its own IP allocator.

#### VpcNetwork (replaces flat PodNetwork)

```rust
struct VpcNetwork {
    /// vpc_name → per-VPC allocator
    vpcs: Arc<RwLock<HashMap<String, VpcAllocator>>>,
    /// Cluster identity for Ghost IPv6 construction
    platform_prefix: u32,
    cluster_id: u32,
}

struct VpcAllocator {
    vpc_id: u16,
    base_ip: u32,
    prefix_len: u8,
    max_hosts: u32,
    next_offset: AtomicU32,
    /// pod_id → (GuestIPv4, GhostIPv6)
    allocations: HashMap<String, (String, Ipv6Addr)>,
}
```

**Key operations**:

```rust
impl VpcNetwork {
    /// Register a VPC for IP allocation
    fn register_vpc(&self, name: &str, vpc_id: u16, cidr: &str);

    /// Allocate IP for a pod in its VPC. Returns (GuestIPv4, GhostIPv6).
    async fn allocate_ip(&self, vpc_name: &str, pod_id: &str)
        -> Result<(String, Ipv6Addr)>;

    /// Release a pod's allocation
    async fn release_ip(&self, vpc_name: &str, pod_id: &str);

    /// Resolve Ghost IPv6 from pod_id
    async fn get_ghost_ipv6(&self, pod_id: &str) -> Option<Ipv6Addr>;
}
```

### Service Proxy Changes

The Pingora-based Service Proxy becomes VPC-aware:

#### VPC-Scoped Routing Table

```
Before: "ClusterIP:port" → ["podIP:targetPort", ...]
After:  "(VpcID, ClusterIP:port)" → ["podGhostIPv6:targetPort", ...]
```

Services are scoped to their VPC. A Service in `frontend-vpc` only routes to Pods in `frontend-vpc`.

**Cross-VPC Services**: If a Service needs to be reachable from another VPC, the VPCs must be peered and the Service explicitly exposed cross-VPC (future enhancement).

#### Service Type Changes

```rust
struct Service {
    // ... existing fields ...
    pub vpc: Option<String>,    // NEW: VPC this service belongs to (default: "default")
}
```

### DNS Changes

DNS resolution becomes VPC-scoped:

```
<service>.<namespace>.svc.cluster.local
```

resolves to the Service's ClusterIP **only if the querying Pod is in the same VPC** as the Service.

**Implementation**: The embedded DNS server checks the source Pod's VPC membership before resolving. Pods can only discover Services within their own VPC (or peered VPCs).

### Firecracker VM Integration

VMs already get isolated TAP networks (`172.16.x.0/30`). With Ghost IPv6:

1. **TAP setup unchanged** — the VM still sees its GuestIPv4 on the TAP interface
2. **Ghost IPv6 assigned** — the Agent constructs the Ghost IPv6 from `(ClusterID, VpcID, GuestIPv4)`
3. **eBPF/iptables enforcement** — outbound packets from the VM are tagged with Ghost IPv6; inbound packets are validated against the VM's expected Ghost IPv6
4. **VPC in PodSpec** — VMs respect the same `spec.vpc` field as container Pods

### Control Plane Responsibilities

#### VPC Creation Flow

1. User calls `POST /api/v1/vpcs` with name and CIDR
2. Server validates:
   - Name uniqueness
   - CIDR format validity
   - **No CIDR overlap** with existing VPCs (atomic check via SlateDB)
3. Server allocates next available VpcID (1..65535)
4. Server persists VPC to `/registry/vpcs/<name>`
5. Server notifies Agents via watch stream

#### Pod Scheduling with VPC

1. Pod created with `spec.vpc: "my-vpc"`
2. API Server validates VPC exists and is Active
3. Scheduler places Pod on a node (VPC doesn't affect placement — all nodes participate in all VPCs)
4. Agent receives Pod, resolves VPC → VpcID
5. Agent allocates GuestIPv4 from VPC's CIDR
6. Agent constructs Ghost IPv6
7. Agent configures container networking with GuestIPv4 (pod sees IPv4) and registers Ghost IPv6 in routing tables

#### VPC Deletion Flow

1. User calls `DELETE /api/v1/vpcs/{name}`
2. Server marks VPC as `Terminating`
3. Server blocks new Pod creation in this VPC
4. Existing Pods continue running (fail-static)
5. When all Pods are terminated, VPC moves to `Deleted`
6. **Cooldown period** (300 seconds) before VpcID can be reused (prevents stale packet delivery)

### Data Plane Enforcement

#### Routing Decision (per-packet)

```rust
fn route_packet(src_ghost: Ipv6Addr, dst_ipv4: Ipv4Addr) -> Result<Ipv6Addr> {
    let src_vpc_id = extract_vpc_id(&src_ghost);
    let src_cluster_id = extract_cluster_id(&src_ghost);

    // Step 1: Intra-VPC — destination in same VPC CIDR?
    let src_cidr = get_vpc_cidr(src_vpc_id);
    if src_cidr.contains(dst_ipv4) {
        return Ok(construct_ghost_ipv6(prefix, src_cluster_id, src_vpc_id, dst_ipv4));
    }

    // Step 2: Cross-VPC — is there a peering + matching VPC?
    for peer_vpc_id in get_peered_vpcs(src_vpc_id) {
        let peer_cidr = get_vpc_cidr(peer_vpc_id);
        if peer_cidr.contains(dst_ipv4) {
            return Ok(construct_ghost_ipv6(prefix, src_cluster_id, peer_vpc_id, dst_ipv4));
        }
    }

    // Step 3: No route — drop
    Err(Error::NoRoute)
}
```

#### Spoofing Prevention

- Source GuestIPv4 in outbound packets MUST match the Pod's assigned GuestIPv4
- Ghost IPv6 structure (version, flags, cluster prefix) validated on every packet
- Pods cannot forge traffic that appears to come from a different VPC

### Fail-Static Guarantees

VPC networking follows K3rs fail-static principles:

| Failure | Impact |
|---------|--------|
| Server crash | Existing VPC connectivity continues. No new VPC creation. |
| Agent crash | Pods keep running. Ghost IPv6 routing tables restored from Agent SlateDB cache. |
| Server + Agent crash | Pods unaffected. Recovery reconciles VPC state from Server SlateDB. |

**Agent Recovery**: On restart, the Agent loads VPC allocations from its local SlateDB cache (key: `/agent/vpc-allocations`). Ghost IPv6 → GuestIPv4 mappings are reconstructed, and the Service Proxy / DNS are rebuilt with VPC awareness.

### Implementation Phases

#### Phase 1: VPC Resource & Type System
- [ ] Add `Vpc` type to `pkg/types/`
- [ ] Add `vpc` field to `PodSpec` and `DeploymentSpec`
- [ ] Add `ghost_ipv6`, `vpc_name` fields to `Pod`
- [ ] Add VPC CRUD API endpoints to `k3rs-server`
- [ ] Create `default` VPC on cluster init
- [ ] SlateDB key prefix: `/registry/vpcs/<name>`

#### Phase 2: Ghost IPv6 Addressing
- [ ] Implement `ghost_ipv6` module in `pkg/network/` with construct/parse/validate functions
- [ ] Add Ghost IPv6 constants to `pkg/constants/` (platform prefix, version)
- [ ] Add ClusterID generation and persistence at server init
- [ ] Unit tests with test vectors (match Sync7 RFC-001 format adapted for ClusterID)

#### Phase 3: VPC-Aware CNI
- [ ] Replace `PodNetwork` with `VpcNetwork` (per-VPC IP allocator)
- [ ] Agent resolves VPC → VpcID on pod creation
- [ ] Allocate `(GuestIPv4, GhostIPv6)` pairs per pod
- [ ] Persist VPC allocations in Agent SlateDB cache

#### Phase 4: VPC-Scoped Service Proxy & DNS
- [ ] Service Proxy routing table keyed by `(VpcID, ClusterIP:port)`
- [ ] DNS resolver checks source Pod's VPC before resolving
- [ ] Services inherit VPC from their namespace or explicit `vpc` field

#### Phase 5: Isolation Enforcement
- [ ] Service Proxy drops cross-VPC traffic (no peering)
- [ ] Firecracker VM: validate Ghost IPv6 on TAP packets
- [ ] OCI containers: iptables/nftables rules per VPC
- [ ] NetworkPolicy enforcement scoped within VPC

#### Phase 6: VPC Peering
- [ ] `VpcPeering` resource type and API
- [ ] Bidirectional and InitiatorOnly peering modes
- [ ] Cross-VPC route injection into Service Proxy
- [ ] DNS cross-VPC resolution for peered VPCs
