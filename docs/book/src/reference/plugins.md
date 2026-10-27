# Plugins reference

Every file, field, flag and screen of the plugin system, with ranges and defaults. For a walk-through
see [Using plugins](../tutorials/plugins.md) and [Writing a plugin](../tutorials/writing-plugins.md). The
protocol a plugin speaks is in [Plugins: design and protocol](../developer/plugins.md). The generated
list of every `helia` flag is in the [command line reference](cli.md).

## Where things live

| What | Where | Notes |
|---|---|---|
| Plugins folder | `<settings>/plugins/NAME/plugin.toml` | One folder per plugin. Helia lists up to 256. |
| Approvals and settings | `<settings>/plugins.toml` | Written by Helia, mode 0600 on Linux and macOS. Holds the checksums you approved and the plugin's settings, which may include a token. |
| Extra plugins | `--plugin PATH` | A folder holding `plugin.toml`, or the manifest itself. Repeat for several. For plugins you are writing. |

`<settings>` is Helia's settings folder: `$HELIA_CONFIG_DIR` if set, else `%APPDATA%\helia` on Windows,
else `$XDG_CONFIG_HOME/helia`, else `~/.config/helia`. A plugin found on the command line wins over one of the same name in the plugins folder.

## The manifest, `plugin.toml`

| Key | Type | Default | Meaning and limits |
|---|---|---|---|
| `name` | text | required | 1 to 32 characters: lower-case letters, digits, `-` and `_`, starting with a letter or digit. Unique. |
| `version` | text | empty | The plugin's own version. At most 64 characters. |
| `api_version` | whole number | required | The protocol version the plugin speaks. This Helia supports `1`. |
| `title`, `description`, `author`, `licence`, `homepage` | text | empty | Shown in lists. At most 200, 4096, 200, 200 and 400 characters. |
| `capabilities` | list | required | Any of `stack-backend`, `frame-op`, `image-op`, `analyse`. Unknown names are ignored with a note. At least one must be known. |
| `executable` | path | required | The program or script, relative to the manifest's folder: no `..`, no absolute path, a regular file after links are resolved. |
| `interpreter` | text | none | A bare program name looked up on the `PATH` that runs `executable` (for example `python3`). Up to 32 characters of letters, digits, `.`, `_` and `-`. The interpreter itself is not part of the checksum; the approval screen names it. |
| `args` | list of text | empty | Fixed arguments after the executable. At most 32, 512 characters each. |
| `files` | list of paths | empty | Further files the program needs. Covered by the manifest checksum. At most 16. |
| `sha256` | 64 hex digits | none | The author's checksum of `executable`. If given it must match, or the plugin is refused as damaged. |
| `signature` | text | empty | Reserved. Helia shows "unsigned". |
| `settings_schema` | path | none | A JSON Schema file for the plugin's settings form. |
| `[resources]` `gpu`, `memory_mb` | yes/no, whole number | `false`, `0` | Hints shown before you approve. They do not limit the plugin. |
| `[limits]` `timeout_s` | whole number | 120 | Seconds without any message before a call fails. 1 to 86400. |
| `[limits]` `max_total_s` | whole number | 86400 | Seconds one call may take in all. 1 to 604800. |
| `[update]` `manifest_url` | text | none | Where newer versions are announced: `https://` only, no user name, at most 400 characters. |
| `[update]` `signing_key` | 64 hex digits | none | The Ed25519 public key the update manifest must be signed with. |
| `[update]` `channel` | text | empty | A release channel, up to 32 characters of letters, digits, `.`, `_` and `-`. |
| `[[steps]]` `id` | text | required | The step's name, same rules as `name`; unique in the plugin. |
| `[[steps]]` `capability` | text | required | `frame-op`, `image-op` or `analyse`; the plugin must declare it. |
| `[[steps]]` `title`, `description` | text | empty | Shown in lists and in the pipeline. |
| `[[steps]]` `params_schema` | path | none | A JSON Schema file for this step's parameters. |

The manifest is at most 256 KiB, each schema file at most 256 KiB, and at most 64 steps are read. A
plugin that declares `frame-op`, `image-op` or `analyse` must list at least one step of that kind.
Unknown keys are ignored with a note, so a newer manifest still loads. `[update]` is only validated by
Helia; fetching and applying updates belongs to the automatic-update mechanism, and an updated plugin
has a new checksum, so it waits for your approval like any other change.

### Settings and parameters

`settings_schema` describes values that belong to the plugin as a whole (a server address, a token).
`params_schema` describes the values of one step in one use (a strength, a number of iterations). Both use
the same JSON Schema subset, which is listed in
[Settings schema](../developer/plugins.md#settings-schema). Helia checks the values against it before it
starts anything, and says which field is wrong and what range it accepts.

## Approval

| State | Meaning |
|---|---|
| `off` | Found, never approved, or switched off. It never starts. |
| `on` | Approved, and the program and manifest still have the checksums you approved. |
| `changed` | The program or the manifest differs from what you approved. It is off until you enable it again. |
| `damaged` | The program does not match the `sha256` its own manifest states. Download it again. |

Helia hashes the program and the manifest every time it starts the plugin. Enabling shows the source, both
checksums, the capabilities, the resource hints and the statement that a plugin runs with your rights, and
asks you to confirm.

## Command line

### `helia plugins`

| Command | Does |
|---|---|
| `helia plugins list [--json]` | Lists every plugin found, its status, capabilities and source. |
| `helia plugins info NAME` | Everything about one plugin: source, checksums, capabilities, limits, update source, steps with their parameters, settings, current values (secrets shown as `(hidden)`). |
| `helia plugins enable NAME [-y\|--yes]` | Shows the approval screen and asks `Enable this plugin? [y/N]`. Without a terminal the question cannot be asked, so it refuses unless `--yes` is given. `--yes` still records the checksums. |
| `helia plugins disable NAME` | Switches it off. Settings are kept. |
| `helia plugins forget NAME` | Removes the approval and the settings. |
| `helia plugins set NAME KEY=VALUE...` | Changes settings, checked against the settings form (numbers must be in range, a choice must be one of its values). A value of `-` for a secret field reads it from standard input, so it stays out of your shell history. |
| `helia plugins test NAME` | Starts the plugin, runs its own check (for example, can it reach its server), and prints the result and its log. The plugin must be on. |

`--plugin PATH` is accepted before any command and may be repeated.

### Plugin steps in `helia stack` and `helia edit`

| Flag | Does |
|---|---|
| `--plugin-step SPEC` | Runs a step of a plugin. Repeat for several; they run in the order given. |
| `--stack-backend PLUGIN` | `helia stack` only. Stacks with this `stack-backend` plugin instead of on this computer. |

`SPEC` is `PLUGIN/STEP`, optionally followed by parameters, optionally preceded by `pre:`:

| Form | Example |
|---|---|
| no parameters | `invert-py/invert` |
| `key=value` list | `stats-py/stats:threshold=0.8` (numbers, `true` and `false` are recognised; everything else is text) |
| JSON object | `dn/denoise:{"strength":0.5,"mode":"fast"}` |
| before Helia's edits | `pre:dn/denoise:strength=0.5` |

Where a step runs depends on its kind:

| Kind | Runs | In `stack` | In `edit` |
|---|---|---|---|
| `frame-op` | on every frame, before scoring and stacking | yes | refused (an edited image has no frames) |
| `image-op` | on the stack after Helia's own sharpening and edits, before colour; with `pre:` before them | yes | yes, mono images only |
| `analyse` | on the result, after everything else | yes, writes `NAME.analysis.json` | yes |

Things to know:

* A plugin that is not on stops the run before any work starts, and names the command that turns it on.
* The parameters are checked against the step's schema before the run starts.
* An `image-op` step after the edits is skipped for FITS output unless `--sharpen-fits` is given, like
  Helia's own edits; use `pre:` to change a FITS stack. A step that changes the picture's size should be
  last, because crops given to `--crop` are in the original pixels.
* `--stack-backend` cannot be used with true colour, calibration masters, field rotation or `--report`:
  they need files, the observing site or per-frame results that stay on the other side. Helia says which
  one and stops.
* The history recorded in the TIFF, PNG or FITS file names the plugin steps that ran.
* With a stack backend, `--timings` and the frame summary are not printed (they describe the local
  pipeline); the line `stacked: by plugin NAME in Ns` is.

### What a saved file says about plugins

When a plugin took part, the saved file's history (`HISTORY` cards of a FITS file, the description of a TIFF or
PNG) holds `Derived by plugin NAME VERSION, program SHA-256 HEX: STEP, STEP` for each plugin, and
`Plugin NAME STEP: {...}` with the record a step returned as `meta` (at most 900 characters). A step that
Helia skipped (an after-edits step on FITS output) is not named.

In FITS files the host also writes `HIERARCH HELIA PLUGIN` and `HIERARCH HELIA PLUGIN VERSION`, and the allow-listed
`HIERARCH HELIA ML ...` cards a step's `meta.provenance` asked for; the list and the checks are in
[the specification](../developer/plugins.md#fits-cards).

### The analysis file

`helia stack` and `helia edit` write `OUTPUT.analysis.json` when an `analyse` step ran. It is a list with
one object per step:

| Field | Meaning |
|---|---|
| `plugin`, `step`, `params` | What ran and with which parameters. |
| `metrics` | Named numbers. At most 1000 are kept. |
| `overlays` | Shapes in pixels of the final image, origin top-left: `circle` (`x`, `y`, `r`), `rect` (`x`, `y`, `w`, `h`), `line` (`x1`, `y1`, `x2`, `y2`), `points` (`points`), `text` (`x`, `y`, `text`), each with an optional `label`. At most 10000 are kept. |
| `notes` | Lines of text. At most 100. |

## Desktop app

**Top bar.** *Plugins* opens or closes the Plugins page. Esc closes it. `helia-gui --plugin PATH` (repeatable,
also `--plugin=PATH`) names a plugin folder for the run.

**The Plugins page.**

| Control | Does |
|---|---|
| *Look again* | Reads the plugins folder again. |
| *Close* | Back to the viewer. |
| The list | One card per plugin found, with a badge (*On*, *Off*, *Changed*, *Damaged*) and its capabilities. A manifest that cannot be used is listed with the reason. Click to choose. |
| *Enable...* (or *Review and enable again...* for *Changed*) | Shows the source, the program, the interpreter, both SHA-256 checksums, the signature state, what the plugin can do, what it asks for, its time limits and update address, and the statement that it runs with your rights. |
| *Enable plugin* | The only control that switches a plugin on. It records the checksums and the time in `plugins.toml`. |
| *Cancel* | Leaves it off. |
| *Disable* | Switches it off. The approval and the settings are kept. |
| *Test* | Starts the plugin, calls its `test` method, and shows *Passed* or *Failed* with its message and its log. A plugin without a check answers that it started and has none. |
| *Forget* | Removes the approval and the settings. |
| The settings form | One control per field of the plugin's settings schema: text, a hidden box for a secret, a path with *Browse*, a slider (a number with a minimum and a maximum) or a number box, a check box, a choice list. A star marks a required field. |
| *Save settings* | Checks every value against the schema, names the field and the range of a wrong one, and writes the settings to `plugins.toml`. |
| *Details* | The same facts as the enable screen, for a plugin that is on. |

**The Plugins card in the Stack step.**

| Control | Does |
|---|---|
| *Stack on*: *This computer*, plugin names | Chooses Helia's own pipeline or a `stack-backend` plugin that is on. |
| *Add a plugin step* | A menu of every step of every plugin that is on. |
| A step's parameter form | The controls of the step's parameter schema, as for settings. |
| *Remove* | Takes the step out of the run. |
| *Plugins...* | Opens the Plugins page. |
| *Plugin results* | The metrics and notes an `analyse` step reported in the last run. |
| *Draw the shapes on the stack* | Draws the step's circles, rectangles, lines, points and text on the stack, in its pixels, with their labels. |

Image steps run on the stack straight after stacking (before the sharpening and edits); frame steps on every
frame; analysis steps on the result. A true-colour stack cannot use image or analysis steps.

## API

`POST /api/v1/jobs` takes two more fields in `config`:

| Field | Type | Default | Meaning |
|---|---|---|---|
| `plugin_steps` | list of text | empty | Steps of plugins to run, each written as on the command line: `PLUGIN/STEP`, optionally `:KEY=VALUE,...` or `:{"json":"object"}`, optionally preceded by `pre:`. At most 16. `frame-op` steps work on every frame, `image-op` steps on the stack (after the edits, or before them with `pre:`), `analyse` steps on the result. |
| `stack_backend` | text | none | A `stack-backend` plugin to stack with instead of the server's own pipeline. Not with `report`, true colour, calibration or field rotation. |

The plugins must be enabled for the user the server runs as (`HELIA_CONFIG_DIR` of the server's
environment), and they run with that user's rights. A request that names a plugin that is off, changed,
unknown or out of range, or a step name that does not exist, is answered `422` with the same sentences as the
command line, before the job is queued. A plugin that fails while the job runs fails the job with the code
`plugin_failed` and the plugin's own what, why and what to do.

The job's `summary` gains:

| Field | Meaning |
|---|---|
| `plugins` | One object per `analyse` step: `plugin`, `step`, `metrics` (name to number), `overlays` (shapes in pixels of the output image) and `notes`. Absent when none ran. |
| `stacked_by` | The stack-backend plugin that made the stack. Absent for a local stack. |

The field-by-field reference is generated: see the [API reference](api.md).

## Errors

Every plugin error is shown in three parts: what happened, why, and what to do. The ones Helia raises
itself:

| Situation | What you see |
|---|---|
| Not enabled | "The plugin NAME is not turned on." Run `helia plugins enable NAME`. |
| Program or manifest changed | "The plugin NAME has changed since you approved it." Review it and enable it again. |
| Checksum differs from its manifest | "The plugin NAME looks damaged." Download it again. |
| Plugin exited | "The plugin NAME stopped unexpectedly." with its exit code and the last line of its log. |
| Plugin silent | "The plugin NAME stopped answering." after `timeout_s` seconds without a message. |
| Plugin broke the protocol | "The plugin NAME sent something Helia cannot read." with the rule it broke. |
| Result does not fit | "The plugin NAME returned a picture that does not fit." with the sizes. |

The plugin's own message is shown when it gave one.

## Environment

| Variable | Meaning |
|---|---|
| `HELIA_CONFIG_DIR` | Where `plugins/` and `plugins.toml` live. Default as above. |
| `HELIA_PLUGIN_API` | Set by Helia to `1` for the plugin process. |
