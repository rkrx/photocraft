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
   checked on 2026-10-10. Model access and API billing depend on the account.
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
   dimensions must equal the planned working surface exactly. There is no aspect tolerance or independent X/Y scaling.

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
transparent new margins in **target document** geometry, then mapped onto the planned valid working surface. The mask protects the whole
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
`version: 2`, `operation`, `prompt`, `model`, typed `options`, `source` and `mask`. Each image is a bounded base64 PNG
with explicit dimensions. Source/result pixels use sRGB. Mask alpha is zero where edits are requested
and opaque where content should remain. Operations are `fill`, `remove` and `expand`; the prompt contains
the complete editing instructions. Responses contain `version`, `image` and optional provider-reported `quality`. Protocol v1 is rejected explicitly; external helpers must update to v2. A backend that does
not implement a requested operation must return an unsupported-operation error.

To extract a separate module:

1. Reuse the `photocraft-ai` wire schema in a Rust helper process and move provider credentials
   and `photocraft-ai-openai` into that helper.
2. Implement `Backend` (including model capabilities) as a bounded, version-checked local IPC client; inject it in the host.
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
  versions, malformed images and unexpected response dimensions fail explicitly. The OpenAI backend validates the explicit working dimensions and quality before uploading. Narrow document aspect ratios are handled by protected padding.
- Original document depth/profile are preserved; only transport pixels are quantized to RGBA8.
  This milestone is not HDR-preserving reconstruction or a guarantee of product accuracy.

## Geometry and quality (2026-10-10)

Settings → AI Integrations has a main model/quality view and **Advanced** processing controls.
API options (`integrations.imageOptions`) and local processing (`integrations.imageProcessing`)
are separate typed structures. They share the existing desktop/web preference store, migration,
validation and Apply/OK/Cancel workflow. Legacy files acquire the following defaults:

| Setting | Default | Effect |
| --- | --- | --- |
| Model | `gpt-image-2.5-sunburst` | Existing model retained |
| Quality | `high` | Explicit API parameter; `auto/low/medium/high/xhigh/max` for documented 2.5 models |
| Working edge limit | 2048 px | Locally compute an explicit valid surface; configurable 1024–2048 |
| Alignment check | on | Check suitable preserved, opaque context |
| Translation correction | on | Correct only a unique reliable global offset |
| Maximum translation | ±4 **working pixels per axis** | Integer search, configurable 0–8; document displacement is offset divided by uniform scale |
| Transition width | 2 **document pixels** | Blending inside authorization only, configurable 0–32; zero disables blending |
| Boundary colour matching | off | Experimental bounded RGB offset from suitable preserved boundary context |
| Diagnostics | off | Keep one capture in memory for explicitly requested viewing/export |

The adapter advertises the exact documented profiles: Image 2.5 supports `xhigh/max`, Image 2
and Image 1 models stop at `high`. Image 1 uses the three standard sizes; Image 2/2.5 allow
custom dimensions. Unknown OpenAI models fail before network access. External backends own their
capability profiles. No unsupported quality downgrade, parameter substitution or automatic retry
occurs. `input_fidelity` is omitted: it is not documented as an adjustable 2.5 parameter. There
are no seed, CFG or sampling-step controls.

The official [image guide](https://developers.openai.com/api/docs/guides/image-generation)
and [edit reference](https://developers.openai.com/api/reference/resources/images/methods/edit)
were checked on 2026-10-10. For custom sizes, edges are multiples of 16, ratio ≤3:1, area
655,360–8,294,400 pixels, edge ≤3840. PhotoCraft additionally retains its lower 2048-edge and
4,194,304-pixel limits; it does not enable experimental larger sizes. The
[prompting guide](https://developers.openai.com/api/docs/guides/image-prompting) uses explicit
size/quality for edits. The image guide states that masks guide the model rather than enforce
pixel-exact changes; local enforcement remains mandatory.

`photocraft-ai::GeometryPlan` maps pixel centres, with integer coordinates denoting centres at
`x + 0.5`. The forward coordinate is `(x + 0.5) * scale + offset - 0.5`; its recorded inverse
is `(working_x + 0.5 - offset) / scale - 0.5`. Both source and generation-mask alpha use this
same mapping. Custom-size providers uniformly upscale small images to their minimum pixel budget
rather than filling half the input with blank padding; larger images retain their scale unless
the working limit requires downscaling. **517 × 600 maps to 752 × 880 at scale 1.4535135,
offset (0, 3)**. Native backends retain 1:1 sampling. Small rounding padding extends edge
samples and is protected by the generation mask.
Only a reduced working limit or a model with fixed sizes requires uniform downscaling. Sampling
uses premultiplied-alpha bilinear interpolation. Returned PNG dimensions must match the explicit
surface, including the actual PNG header. No stretching, perspective correction or local warp.

For Expand, the original→expanded-document anchor translation precedes this common mapping.
The exported protected rectangle records that translation. The original native layers are only
translated, never resampled or replaced by provider pixels, even when the uploaded preview is
downscaled. The original selection is restored; existing stale/cancel/undo guards still govern
publication of the complete canvas/layer change.

The generation mask is a separate working-space image. The insertion mask remains in document
space, and native-depth selection coverage is enforced again after colour conversion. Currently
no extra generation-context dilation is requested; adding one later cannot enlarge local insertion.
Protected padding is excluded from alignment. Outside authorized coverage, the new layer has
zero alpha, so original pixels and native precision are preserved.

Alignment compares at most about 4096 grid samples, excluding editable neighbourhoods,
working padding and alpha below 0.99. Fewer than 64 samples or mean RGB-channel variance below 0.0004
produce **insufficient context**, not failure. Translation candidates use mean-centred RGB
correlation per channel, weighted by source variance. Positive contrast/exposure/white-balance
changes do not constitute geometric deviation. A plausible identity or corrected candidate
requires correlation ≥0.90; correction additionally requires a gap >0.05 to the next candidate.
Only a best correlation below 0.35 rejects substantially incompatible preserved context;
intermediate/ambiguous evidence is **insufficient context** and is not automatically rejected or
corrected. No usable comparison is also insufficient context. Diagnostics retain both
`correlation` (absent in older captures) and the colour-offset-adjusted RGB residual `error`;
the residual alone is never used to reject geometry. These are synthetic-test thresholds,
not a semantic geometry guarantee or a real-provider calibration. Periodic/ambiguous
textures remain unverified; local changes between samples may escape detection. A corrected
global translation says nothing about repairing local deformation. Disabling correction leaves
a detected shift unapplied. A correction that leaves edited pixels without coverage is rejected.

Transitions blend generated RGB with opaque original context **inside hard selections**; soft
selection alpha is retained without a second feather. For Expand, only opaque locally flat edge
colours extend into the first new margin pixels; textured/transparent edges are not extrapolated.
This avoids fading margins into transparency or copying sharp edge structures along a border.
The optional colour matcher measures local colour residuals only in protected opaque context,
using bounded 8-pixel cells and 64 diffusion passes. It requires reliable alignment, caps each
channel correction at 0.12, and fades its influence within 32 edited pixels. Opposite local
colour casts no longer cancel into a single global average. It stays off by default; the CLI
acceptance comparison explicitly enables it. It cannot repair incorrect generated geometry. Sharpening and grain matching are deliberately not
exposed: new semantic content, transparent borders and small context do not yet provide a robust
reference for estimating a safe sharpening kernel or noise spectrum. No inert switches are added.

Every job snapshots API/local options before upload. Command/job results include requested
quality, working size, transform and alignment. Later preference edits affect only later jobs.
Diagnostic responses separately retain reported resolved quality, when provided; `auto` never
fabricates a known resolved quality. A reported quality inconsistent with a specific request fails.

## Explicit diagnostics and offline processing

1. Enable **Keep diagnostics for explicit export** under Advanced before the desired job.
   Confirm with **Apply** or **OK** before generating. Earlier attempts made with capture off
   were not retained and cannot be exported retroactively.
2. Open **Generation diagnostics…** from AI settings (automation: `ai.diagnostics.show`).
3. Select document source, transmitted source, generation alpha mask, raw response, mapped result,
   insertion alpha mask or final preview. Mask transparency uses the configured checkerboard;
   transparent means editable, opaque means protected.
4. **Export diagnostics…** writes a bounded JSON bundle only after choosing a destination.
   The separate PNG stages, full immutable request/response, prompt, requested/reported quality,
   dimensions, scale/offset, protected original rectangle, alignment and elapsed native runtime
   are included. No API keys or HTTP headers. These are sRGB RGBA8 diagnostic previews;
   native documents retain their own profile/depth. Native runtime is wall-clock job processing;
   browser captures use 0 when a clock is unavailable.
5. **Import diagnostics…**, change local settings as desired, then **Replay offline**.
   It reprocesses the saved response with those explicit local settings without HTTP and without
   modifying the document. Automation uses `ai.replay {capture?, local?}`. Omit `local` to retain
   the saved settings. Successful replay sets `applied:false`: a preview is distinct from a
   committed layer. Failed local processing retains the raw bounded response and error.
6. `ai.diagnostics {}` returns the latest capture; `{clear:true}` releases it. One capture replaces
   the previous one, including concurrent documents' latest completed processing. Export/import
   commands with `path` use the control session's authorized read/write services. Interactive
   actions use existing native pickers/web downloads. Export/replay are disabled without a
   capture/response; picker/write errors and the successfully saved path appear inside the
   diagnostic dialog, including asynchronous completions. Cancel writes nothing.

Encoded capture/export size is capped at 96 MiB, PNG/decoder limits also apply. A single selected
preview texture is cached and replaced; images are not decoded on every frame. These are bounded
allocations, not a total RSS guarantee. Failed geometry keeps diagnostic inputs/response when
bounded; oversized/malformed transport is rejected. Stale results never publish a document edit;
`applied` is set only after the job revision guard accepts the new layer. Explicitly clear captures
when they are no longer needed. Diagnostic JSON contains private image/text content if you choose
to export it; the app never exports it automatically.

A fully synthetic end-to-end fixture, stage PNGs, offline replay and a prepared high/xhigh pair:

```sh
cargo run -p photocraft-engine --example generative_offline -- --out log/generative-offline/fill --shift 2
cargo run -p photocraft-engine --example generative_offline -- --out log/generative-offline/remove --operation remove
cargo run -p photocraft-engine --example generative_offline -- --out log/generative-offline/expand --operation expand
```

The example never connects to any API. `comparison-high-xhigh.json` (engine command
`ai.comparison`) contains identical source, alpha mask, prompt, model and requested dimensions;
only `options.quality` changes. Keep local processing identical when evaluating the two saved
responses. Explicitly authorize two billable requests before using this pair with a real provider.
Compare seam error, geometry/shift status, thin structures, texture/alpha, latency and cost. A
synthetic reconstruction demonstrates local invariants, not better output from a real model.

## Direct CLI generation testing

`photocraft-cli generative-fill`, `generative-remove` and `generative-expand` are file-oriented
adapters around the existing `edit.generativeFill`, `edit.generativeRemove` and
`edit.generativeExpand` engine commands. They share input/output handling, the same provider,
geometry checks, local processing and layer insertion as the UI, without a second imaging pipeline.
The image need not be opened in a running PhotoCraft window.

```sh
cargo build -p photocraft-cli
./target/debug/photocraft-cli generative-remove /Users/rkr/Projects/craft-package/original.png \
  --mask /Users/rkr/Projects/craft-package/mask.png \
  --out /Users/rkr/Projects/craft-package/removed.png \
  --prompt "bottle" --quality high --consent \
  --diagnostics /Users/rkr/Projects/craft-package/remove-diagnostics
```

Fill uses the same image/mask/output flags and requires a nonempty `--prompt` describing the
new content. Expand needs no mask: use `--width <px> --height <px>` or `--aspect 16:9` for the
smallest containing banner. Optional `--anchor` accepts `center` (default), `topLeft`, `top`,
`topRight`, `left`, `right`, `bottomLeft`, `bottom` and `bottomRight`; its optional prompt describes
the scene to continue. Dimension/anchor validation reuses the engine helpers before any upload.

```sh
./target/debug/photocraft-cli generative-fill input.png --mask mask.png \
  --prompt "Replace the bottle with a small white ceramic jug; preserve everything else" \
  --out filled.png --consent
./target/debug/photocraft-cli generative-expand input.png --aspect 16:9 \
  --anchor center --out expanded.png --consent
```

Provide `OPENAI_API_KEY` through the environment. `--consent` authorizes uploading the visible
image and mask for this invocation. Each invocation makes at most one generation request; the
adapter adds no retries. `--prompt` is an optional object description appended to the engine's
removal template. `--model` overrides the default `gpt-image-2.5-sunburst`; `--quality` accepts
`auto`, `low`, `medium`, `high`, `xhigh` or `max` when supported by that model, with `high` the
default. These are generation settings, not JPEG export quality. A CLI session uses engine
defaults plus its explicit flags; it does not read or modify the UI's persisted preferences.

The mask must have the same document dimensions as the input: white selects the maximum
editable area, black protects it, and gray supplies partial coverage. It is loaded through
the engine's composite-channel selection command, which handles document color/depth conversion.
This is a black/white selection mask, not the API's inverse-alpha mask. Use PNG for lossless
pixel comparisons or `.pcraft`/PSD to retain the original document and generated layer. Lossy
exports can change protected pixels during encoding even though the engine protects them.
Existing output files require `--overwrite`; input, mask and recorded response paths are never
overwritten. The output's parent directory must already exist.

For an offline test, add `--response /absolute/path/to/response.png` instead of `--consent`.
This bypasses the HTTP backend and needs no API key, while retaining the same engine processing
and application checks. The PNG must be the raw response in the engine's **working geometry**,
not the original document dimensions. The adapter does not resize an incompatible response.
Responses from experiments with a different crop/scale/padding plan cannot simply be substituted.
Reported resolved quality is left unknown for a recorded response; the requested quality remains
in the capture.

`--mask-mode auto|api|reference` controls only the provider transport. Auto uses an opaque
black/white selection-reference image for Fill/Remove (validated on Sunburst), and the API alpha
mask for Expand. Reference mode submits the source as the first image and the opaque selection
guide as the second, with explicit guide-only instructions; API mode retains the alpha-mask
request. Local insertion always uses the original selection independently of provider adherence.
`--processing` accepts a typed local-options JSON object, for example
`{"matchBoundaryColor":true,"transitionWidth":5}`. It permits identical processing settings for
live and offline comparisons without modifying persisted UI preferences. Unknown fields and
out-of-range values fail. Colour matching remains optional; no image-specific parameter tuning
is used in the acceptance comparison.

`--diagnostics` names a **new directory**. It receives `capture.json`, the source and inverse-alpha
wire mask, the opaque `reference-mask.png` when used, `transport.json`, the raw response when
available, and the local stage PNGs. Captures
remain compatible with `ai.replay`. They contain image/prompt content and no credentials.
The adapter exports captures after a failed generation or geometry check too; a failure before
capture preparation writes JSON `null`. Exit codes follow the CLI convention: 0 success,
1 operation/file failure, 2 unknown command/flag or malformed flag usage. Success prints JSON
with the engine result (requested quality, working size, geometry and alignment), save warnings
and diagnostic path. This interface makes current behavior reproducible; it does not by itself
resolve every real-image quality limitation.

### CLI acceptance evidence (11 October 2026)

Eleven successful billable CLI requests used the user-authorized banquet photo and rectangular
mask, pinned `gpt-image-2.5-sunburst-2026-09-08`, no retries: three legacy baselines; two independent
reference runs each for Remove/Fill; one reference Expand; and continuity-template API-mask
Expand at high/xhigh/max. No public sample or personal photo is committed. Inputs, raw responses,
commands, captures and comparisons are retained locally in `log/generative-cli-live/2026-10-11/`.

The baseline Remove/Fill erased adjacent red flowers/clothing within the loose selection. Both
reference repeats remove/replace the bottle while retaining those surrounding objects, with
visually compatible transitions and focus on this photo. Additional reconstructed tabletop
objects are allowed. The optional colour correction was then evaluated offline on the same
responses; no further uploads were needed. All 18 exported results preserve protected pixels
exactly: outside the selection for Fill/Remove, and throughout the original rectangle for Expand.

Expand has **not passed visual acceptance**: xhigh/max and the refined local colour correction
reduce some differences, but the new margins still show join/geometry differences at the
original edges. Successful transport, plausible sampled alignment and exact protected pixels
do not prove a seamless expansion. No quality setting is recommended as a reliable Expand fix.
This one-photo evaluation is not general reliability evidence for cables/people/product photos.

Per-run duration and call counts are recorded. The current provider response boundary retains
image/resolved quality, not token-usage/billing metadata; exact charges are therefore not inferred.
See the local `assessment.json` and `pixel-verification.json` for run-level evidence.

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
