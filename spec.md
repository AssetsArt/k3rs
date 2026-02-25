# K3rs: A Lightweight Scheduling & Orchestration Platform

## Overview
This document outlines the design and architecture for a new Scheduling & Orchestration system written in Rust (`k3rs`). It is heavily inspired by the minimal, edge-focused architecture of [K3s](https://k3s.io/). A core differentiator of this project is the extensive integration of [Cloudflare Pingora](https://github.com/cloudflare/pingora) as the primary engine for networking, proxying, and API routing.

## Goals
- **Minimal Footprint**: Single binary execution for both Server and Agent, similar to the K3s model.
- **High Performance & Safety**: Built natively in Rust for memory safety and extreme performance.
- **Advanced Networking**: Deep integration of Pingora for all Layer 4/Layer 7 routing, reverse tunneling, and API gateway features.
- **Edge Native**: Designed for resource-constrained environments, IoT, and Edge Computing scenarios.

## Architecture Structure

The system follows a classical Control Plane (Server) and Data Plane (Agent) architecture.

### 1. Server Components (Control Plane)
The server binary encapsulates all control plane processes:
- **Supervisor**: The init process managing the lifecycle of all internal processes and threads.
- **API Server (powered by Pingora)**: The central entry point for all control plane communications. Instead of a traditional HTTP server, Pingora acts as a high-performance, programmable REST/gRPC API gateway layer.
- **Scheduler**: Determines which node (Agent) a workload should run on, based on resource availability, node labeling, and constraints.
- **Controller Manager**: Runs background control loops to maintain the desired state of the cluster (e.g., node liveness, workload deployments).
- **Data Store**: Embedded database built on object storage using [SlateDB](https://slatedb.io/) for robust, cost-effective, and highly available state persistence.

### 2. Agent Components (Data Plane)
The agent binary runs on worker nodes and executes workloads:
- **Tunnel Proxy (powered by Pingora)**: Maintains a persistent, secure reverse tunnel back to the Server (similar to K3s). Pingora's connection pooling and multiplexing capabilities make it ideal for managing these reverse tunnels dynamically without dropping packets.
- **Agent Node Supervisor (Kubelet equivalent)**: Communicates with the Server API, manages container lifecycles, and reports node resource utilization and health status.
- **Container Runtime Integrator**: Interfaces directly with `containerd` via gRPC to pull required images, and start, stop, and monitor containers/pods.
- **Service Proxy (powered by Pingora)**: Replaces `kube-proxy`. Uses Pingora to dynamically manage advanced L4/L7 load balancing for services running on the node, routing traffic seamlessly to the correct local or remote Pods.
- **Overlay Networking (CNI)**: Manages pod-to-pod networking (similar to Flannel or Cilium).

## Why Cloudflare Pingora?
Using Cloudflare Pingora as the backbone for this orchestrator provides several architectural advantages:
- **Memory-Safe Concurrency**: Pingora handles millions of concurrent connections efficiently, avoiding memory leaks typical in C-based proxies.
- **Unified Proxying Ecosystem**: It replaces multiple discrete components (API Gateway, Ingress Controller, Service Proxy, Tunnel Proxy) with a single unified, programmable Rust framework embedded directly into the binary.
- **Dynamic Configuration**: Pingora allows hot-reloading of routing logic and proxy rules without dropping existing connections, which is crucial for a fast-churning orchestration environment where services are constantly scaling.
- **Protocol Flexibility**: Native support for HTTP/1.1, HTTP/2, TLS, and Raw TCP/UDP streams, making it perfect for both cluster internal communications and exposing external workloads.

## Implementation Phases

### Phase 1: Core Foundation & Communication
- [ ] Initialize Rust workspace and configure `pingora` dependencies.
- [ ] Implement Pingora-based API Server stub to accept registration requests.
- [ ] Implement Pingora-based Tunnel Proxy to establish bi-directional communication between Agent and Server.
- [ ] Implement basic state DB using SlateDB over S3-compatible object storage.

### Phase 2: Orchestration Logic
- [ ] Implement Node Registration and health-check ping mechanisms.
- [ ] Define cluster object primitives (Workloads, Pods, Services) using Serde/JSON.
- [ ] Implement a basic Scheduler (resource-aware or round-robin node assignment).
- [ ] Connect Agent to `containerd` using `tonic` gRPC clients to pull images and start simple containers.

### Phase 3: Networking & Services
- [ ] Implement the Pingora-based Service Proxy (kube-proxy alternative) on Agents.
- [ ] Pod-to-Pod networking setup (integrate with a lightweight CNI or write a custom eBPF/Veth router).
- [ ] Distribute dynamic routing updates from Server to Agents whenever a new service/pod is created.

### Phase 4: Reliability & Extensibility
- [ ] Implement advanced Controller loops (ReplicaSets, Deployments, DaemonSets).
- [ ] Implement graceful node shutdown and Pingora zero-downtime proxy upgrades.
- [ ] Add Metrics and Observability endpoints (Prometheus integration).

## Tech Stack
- **Language**: Rust
- **Networking/Proxy/Ingress**: `pingora` (Cloudflare)
- **Container Runtime**: `containerd` (communicating over `tonic` gRPC)
- **Storage**: `slatedb` (Embedded database built on object storage)
- **Serialization**: `serde`, `prost` (Protocol Buffers)
- **Async Runtime**: `tokio` (Pingora and Tonic dependency)
