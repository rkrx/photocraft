# photocraft-ai

Provider-independent protocol v1 for generative image operations. Implements bounded PNG transport,
request validation and the `Backend` trait. It depends on no other workspace crate, filesystem,
HTTP client or UI. The implemented operation is `Fill`.

See [the integration guide](../../docs/generative-ai.md) for capabilities, host responsibilities,
credentials and how to replace an in-process backend with an IPC client.
