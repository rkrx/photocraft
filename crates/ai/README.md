# photocraft-ai

Provider-independent protocol v2 for generative image operations. Implements bounded PNG transport,
typed quality/working limits, model capability profiles, exact uniform geometry planning
(including a shared image/mask upscale for small custom-size inputs),
request validation and the `Backend` trait (including capability discovery). It depends on no other workspace crate, filesystem,
HTTP client or UI. The implemented operations are `Fill`, `Remove` and `Expand`. A request carries the complete editing
instructions in `prompt`; `Remove` asks for object removal and background reconstruction,
while `Expand` continues a padded source into its editable margins at the target dimensions.

See [the integration guide](../../docs/generative-ai.md) for capabilities, host responsibilities,
credentials and how to replace an in-process backend with an IPC client.
