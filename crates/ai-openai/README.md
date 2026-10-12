# photocraft-ai-openai

Built-in native OpenAI image-edit backend implementing `photocraft_ai::Backend`. `OpenAi::new`
accepts a session credential; hosts may call `from_env` for `OPENAI_API_KEY`. No engine, document,
filesystem or UI dependencies. Wasm builds do not expose the native provider.

Size and quality are explicit, validated against the documented concrete model profile before upload.
The response retains provider-reported quality when available. Unknown models fail; no downgrade
or parameter-changing retries are attempted. `input_fidelity` is omitted.

The endpoint is fixed, redirects/proxies are disabled, requests are bounded and credentials/error
bodies are not logged. All operations use the image-edit endpoint. Fill/Remove default to an opaque selection guide
as a second reference image, with guide-only framing/style instructions around the editor prompt.
Expand defaults to the API alpha mask. `MaskMode` and `from_env_with_mask_mode` allow controlled
transport comparisons; `selection_reference` performs bounded alpha inversion using the standalone
PNG codec (no editor pixel assumptions). The original wire mask still controls local insertion.
These choices were evaluated on one photo, not proved reliable on arbitrary inputs. Cancellation prevents applying the result; it cannot guarantee cancelling
remote charges. See [the integration guide](../../docs/generative-ai.md).
