# ClawPod

`ClawPod` lives under `experiments/clawpod` as an independent Rust workspace.

## Layout

- Workspace: `experiments/clawpod/Cargo.toml`
- Crates: `experiments/clawpod/crates/{runtime,agent,domain,config,queue,routing,runner,team,session,store,telegram,discord,slack,observer}`
- App binary: `experiments/clawpod/crates/runtime`

## Quick Start

```bash
cd experiments/clawpod
cargo run -p runtime -- doctor
cargo run -p runtime -- daemon
```
