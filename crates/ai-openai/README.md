# photocraft-ai-openai

Built-in native OpenAI image-edit backend implementing `photocraft_ai::Backend`. `OpenAi::new`
accepts a session credential; hosts may call `from_env` for `OPENAI_API_KEY`. No engine, document,
filesystem or UI dependencies. Wasm builds do not expose the native provider.

The endpoint is fixed, redirects/proxies are disabled, requests are bounded and credentials/error
bodies are not logged. Both Fill and Remove use the masked image-edit endpoint; the provider
forwards the complete instructions supplied by the editor. Cancellation prevents applying the result; it cannot guarantee cancelling
remote charges. See [the integration guide](../../docs/generative-ai.md).
