# Generative AI in this fork

This fork embeds image generation into PhotoCraft while keeping the provider replaceable.
It implements **Generative Fill**, **Generative Remove** and **Generative Expand**. The remaining requested operations
are future milestones, not advertised capabilities. The built-in OpenAI backend is desktop-only;
the web build compiles and explains that no provider is installed. Existing WASM filter plug-ins are unchanged.

## Use

1. Open **Edit → Preferences → AI Integrations…**, or the AI Integrations page inside the general
   Settings dialog. Set the model and enter an OpenAI API key, then click Apply or OK. The model
   is saved with normal preferences; the key is kept only for the running session. The default is
   `gpt-image-2.5-sunburst`, the current precise-editing model in the official documentation
   checked on 2026-10-09. Model access and API billing depend on the account.
2. Open an RGB, Gray, CMYK or Lab image, up to 2048 × 2048 pixels, and make a selection.
3. Choose **Edit → Generative Fill…** and enter a description, or **Edit → Generative Remove…**
   to remove selected objects. Both are also in the selection context menu. For removal, include
   the object, its shadow and a small margin of background in your selection. An optional object
   description can clarify what to remove; no text is required. The provider is instructed to
   reconstruct the background from surrounding context rather than add replacement objects.
   Provider and model are shown read-only; the AI Integrations button opens the central settings.
4. Confirm sending the **entire visible canvas** and the selection mask to the provider, then
   click Generate or Remove. The canvas is colour-converted to sRGB, encoded as PNG, and sent without
   document metadata, hidden layers, paths, credentials or the document file.
5. The result becomes one new raster layer above the root layer stack and one undo step,
   even when a layer inside a hidden group is active. PhotoCraft applies the original
   feathered selection locally: changes outside it cannot affect the composition. Generated
   dimensions are fitted to the canvas; aspect-ratio changes over 3% are rejected.

For **Generative Expand**, choose **Edit → Generative Expand…** without needing a selection.
Use **16:9 Banner** or enter a target width/height and choose one of the nine anchors to place the
original. For example, 900 × 900 becomes 1600 × 900 at unchanged image scale. The preset chooses
the smallest exact 16:9 canvas containing the image; integer rounding can add a few rows as well
as columns (1024 × 1024 becomes 1824 × 1026). It never shrinks or rescales the source. An optional
scene description directs how the background continues. Confirm upload, then click Extend.

**Remember last text per AI dialog** in AI Integrations is enabled by default. Fill, Remove
and Expand each restore their own last submitted text, including after restarting PhotoCraft.
Confirming a valid action remembers its text even if the provider later fails or the job is
cancelled; cancelling an unsubmitted dialog keeps the previous text. Submitting an empty
optional text clears that dialog's previous text. Upload consent is always unchecked again.

These texts use the existing `Preferences.dialogs` store, keyed by the operation's command id,
with only `{"prompt":"…"}` saved per dialog. Desktop hosts atomically write `preferences.json`
in the normal macOS/Windows/Linux configuration directory (or the configured/portable data
directory); the Rust web host uses the existing `photocraft.preferences` localStorage entry.
The texts are local, unencrypted user settings, not document contents or provider credentials.
Turning remembering off with Apply/OK clears the saved AI texts and opens these dialogs empty;
turning it back on does not restore old texts. Other dialogs' remembered settings are preserved.
The existing preference save/error/retry and multi-window merge paths are reused. Invalid or
oversized saved texts are ignored. Direct engine/API calls do not change UI text memory.

Both target dimensions must be positive integers, at least as large as the original, with at least
one side growing, and no side above 2048. An oversized 16:9 preset is disabled with an explanation;
custom dimensions are still available where they fit. The full visible source is padded with
transparent new margins, then uploaded at the **target** dimensions. The mask protects the whole
original rectangle, including transparent pixels, and requests edits only in the new margins.
Existing selections do not restrict expansion; they are translated and restored afterward.

The generated raster layer sits above the original layers and is transparent inside the original
rectangle. Original raster pixels keep their native precision and are only translated, never
resampled or replaced with provider pixels. Layers/masks, text/vector geometry, saved paths,
channels, guides and document marks follow the existing Canvas Size translation logic. Before
upload, PhotoCraft compares the original area's composition after that geometry change. If
canvas-relative fills/effects would change it, the operation fails explicitly: expand a flattened
copy instead. Incomplete/transparent generated margins are also rejected so previously hidden
pixels cannot show through. Canvas, translated content, selection and generated layer commit
together as one undo step; a failed or cancelled request leaves the original canvas intact.

Existing `OPENAI_API_KEY` configuration can be used by the desktop app, CLI run/batch/droplet,
headless control server and headless MCP. Bridge mode uses the desktop session's provider.
There is no key argument or key-bearing engine command. Keys entered in Preferences are excluded
from serialized UI state, inspection and debug output; they are neither saved in preferences nor
written to documents, journals, logs or command recordings. Stored OS-keychain credentials and
other login methods are not implemented. A ChatGPT subscription is not an API credential.

Agents call `ai.status` to inspect availability and `edit.generativeFill` with:

```json
{"prompt":"Place a small red vase on the table","model":"gpt-image-2.5-sunburst","consent":true}
```

For removal, call `edit.generativeRemove` with:

```json
{"consent":true}
```

Or specify the object and optionally override the model:

```json
{"prompt":"The cable and its shadow","model":"gpt-image-2.5-sunburst","consent":true}
```

Removal accepts an optional `prompt` of up to 31,000 characters. The engine combines it with
provider-independent removal/background-reconstruction instructions and sets the wire operation
to `remove`. Fill keeps the operation `fill` and requires a non-empty prompt. This does not add
automatic object detection or a brush tool; existing selection tools define the editable region.

For expansion, call `edit.generativeExpand` with:

```json
{"width":1600,"height":900,"anchor":"center","prompt":"Continue the neutral studio background","consent":true}
```

`prompt` and `anchor` are optional (empty description and centre by default); `model` uses the
central preference unless explicitly overridden. Anchors are `topLeft`, `top`, `topRight`,
`left`, `center`, `right`, `bottomLeft`, `bottom` and `bottomRight`. The command returns
`layer`, `operation`, `width`, `height` and `offset`. Wire operation `expand` uses a padded source
and mask at the target dimensions, so a future helper needs no editor geometry or document types.

The dialog uses the normal `ui.menu.invoke`, `ui.dialog.set` and `ui.dialog.confirm` control
methods; its prompt and upload consent are visible state. The model preference is
`integrations.imageModel`; text remembering is `integrations.rememberDialogTexts` (boolean, default
`true`). Both are accessible through `prefs.get` / `prefs.set`. All three operations use that
default when their engine command omits `model`; an explicit command model overrides it for one
request. Settings Apply/OK validates both the preference draft and any replacement provider before
installing them. Cancel discards un-applied model changes and transient key input; existing connected
providers remain active. Invalid settings keep the draft open for correction. Automation obtains credentials
from the host environment, not the dialog JSON. A recorded or batched invocation is a new provider
request, potentially with a new charge; generation is not deterministic. No automatic retries.

## Boundary and later extraction

| Component | Responsibility | Dependencies |
|---|---|---|
| `photocraft-ai` | Versioned serializable request/response, encoded PNG data, limits, provider trait | Serde, base64, error type; no workspace dependencies |
| `photocraft-ai-openai` | API authentication and HTTPS image edits | AI contract and native HTTP transport; no engine or UI |
| `engine::generative_cmds` / `generative_expand_cmds` | Snapshot, colour management, masks, canvas geometry, result validation, insertion, undo | AI contract and existing editing crates |
| `ui-egui::generative_ui` / `prefs_ui` | Prompt/consent dialog and central AI preferences with transient key input | AI contract and engine; no HTTP or OpenAI dependency |
| Desktop / CLI hosts | Inject the backend into `Session::generative_backend` | OpenAI backend |

`Backend::generate(Request, cancellation)` operates on transport data only. Requests contain
`version`, `operation`, `prompt`, `model`, `source` and `mask`. Each image is a bounded base64 PNG
with explicit dimensions. Source/result pixels use sRGB. Mask alpha is zero where edits are requested
and opaque where content should remain. Operations are `fill`, `remove` and `expand`; the prompt contains
the complete editing instructions. Responses contain `version` and `image`. A backend that does
not implement a requested operation must return an unsupported-operation error.

To extract a separate module:

1. Reuse the `photocraft-ai` wire schema in a Rust helper process and move provider credentials
   and `photocraft-ai-openai` into that helper.
2. Implement `Backend` as a bounded, version-checked local IPC client; inject it in the host.
3. Map progress/cancellation, define authentication, install/update the helper and document its
   permissions. The editor's colour, selection and history code remains the same.

This is an extraction seam, **not an existing external plug-in protocol**. The editor chooses
what data leaves the process. A helper process would not itself constitute a security sandbox.
The synchronous provider trait currently matches native worker jobs; a browser implementation
would need asynchronous job integration and a separate credential/security design.

## Failure and resource policy

- Native generation uses the existing background-job system. While running, the source document
  is locked against edits. Changed or closed documents reject late results; cancelled and failed
  jobs do not add history or layers.
- Cancellation discards the local result immediately. An HTTP call already in flight may finish
  remotely and incur a charge; the worker has a 180-second request timeout and a 15-second connect
  timeout. HTTP always runs on its own thread, including callers inside an async MCP executor.
- The OpenAI endpoint is fixed to `https://api.openai.com/v1/images/edits`; redirects and system
  proxies are disabled. Raw HTTP errors and provider response bodies are not echoed.
- The wire limit is 12 MiB per encoded PNG (before base64), less than 4 MiB for the mask, and
  20 MiB for an HTTP response. Decode limits enforce 2048-pixel edges, 4,194,304 pixels and a
  64 MiB pixel allocation budget. These are operation limits, not a total process-RSS guarantee.
- Large documents, empty selections for Fill/Remove, unsupported colour modes, invalid parameters, protocol
  versions, malformed images, mismatched dimensions and changed aspect ratios fail explicitly. The OpenAI backend rejects source aspect ratios
  outside 1:3–3:1 before uploading.
- Original document depth/profile are preserved; only transport pixels are quantized to RGBA8.
  This milestone is not HDR-preserving reconstruction or a guarantee of product accuracy.

## Incremental milestones

Each estimate is human specialist work including focused tests and review, with a maximum of
five hours. Evaluate each result together before authorizing the next milestone; split it further
if a dependency or quality problem exceeds that scope.

| Milestone | Scope | Estimate |
|---|---|---|
| 1 (implemented) | Embedded boundary, OpenAI backend, integrated Generative Fill, failure/undo tests | 4–5 h |
| 2 (open) | Real API acceptance with a deliberately chosen non-personal image; provider dimensions, cancellation and result-quality review | 1–3 h |
| 3 (implemented) | Generative Remove using the shared selection/request/result path; optional object description and dedicated wire operation | 2–3 h |
| 4 (implemented) | Generative Expand: target canvas, exact 16:9 preset, anchors, original-image preflight and atomic canvas/layer undo | 3–5 h |
| 5a | Background workflow: assess existing subject selection and define background mask | 2–3 h |
| 5b | Generate/replace background through that mask | 2–4 h |
| 6 | Harmonize a selected inserted object; compare against surrounding light/colour | 3–5 h |
| 7 | Generate a new image from text into a new document | 2–4 h |
| 8a | Generative upscale capability/quality experiment and acceptance criteria | 2–3 h |
| 8b | Implement the accepted upscale path, or report unsupported backend capability | 3–5 h |
| 9 | Variations and reviewable previous results | 3–5 h |
| 10 | Explicit reference-image selection and bounded transport | 3–5 h |
| 11a | Perspective-change experiment with product-accuracy acceptance criteria | 2–3 h |
| 11b | Implement the accepted perspective workflow | 3–5 h |
| 12 | Retouch workflow and before/after evaluation | 3–5 h |

Provider quality/capability experiments are gates, not promises that every operation can preserve
the real geometry or details of a photographed product. Preview/variant management, cropped
large-image processing, persisted secure credentials and independent backend installation remain
open. Fill, Remove and Expand have synthetic workflow and local HTTP checks; live API acceptance and
removal quality on cables/people and expansion quality on product backgrounds are still open. A mock reconstruction
proves application, masking and undo, not the quality of a remote model’s reconstruction.

Official API references checked for this implementation:
[Image generation](https://developers.openai.com/api/docs/guides/image-generation) and
[Image edits](https://developers.openai.com/api/reference/resources/images/methods/edit).
Tests use synthetic images and local mock HTTP, without an actual OpenAI upload or charge.
