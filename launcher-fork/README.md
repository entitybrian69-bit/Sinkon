# MojoLauncher fork integration

Sinkon ships as one Gradle module plus a patch against
[MojoLauncher](https://github.com/MojoLauncher/MojoLauncher) (`v3_openjdk`,
`RenderSpec`-based renderer selection).

* `patches/0001-register-sinkon-renderers.patch` — a real `git diff` generated
  against `v3_openjdk` (verified to reverse-apply cleanly against that revision).
* `app_mojo/.../impl/SinkonRenderSpecs.kt` — `SinkonVulkanRendererSpec`,
  `SinkonGLRendererSpec`, `SinkonAutoRendererSpec`.
* `app_mojo/.../impl/SinkonSurfaceBridge.kt` — surface lifecycle → `SinkonSession`.
* `app_mojo/src/main/res/values/strings_sinkon.xml` — renderer names.

The patch already contains these files; they are kept here as plain sources so they
can be reviewed without opening a diff.

## The diff, in words

| File | Change |
| --- | --- |
| `RenderSpec.java` | + `default int backendHint()` (`-1` = not applicable), + `default int badge()` (`0` = launcher default icon) |
| `def/Renderers.java` | + `SINKON_VULKAN_RENDERER = "sinkon_vulkan"`, `SINKON_GL_RENDERER = "sinkon_gles3"`, `SINKON_AUTO_RENDERER = "sinkon_auto"` |
| `GameRenderer.java` | + 3 cases in `getKnownRenderer()` (plus the imports the switch needs) |
| `RendererCache.java` | + the 3 tags in the compatibility list, so they show up in the settings picker |
| `GameView.java` | + 3 one-line hooks: `onSurfaceAvailable`, `onSurfaceResized`, `onSurfaceDestroyed` → `SinkonSurfaceBridge` |
| `app_mojo/build.gradle` | + `org.jetbrains.kotlin.android` plugin, + `kotlinOptions`, + `implementation project(":sinkon-native")` |
| `settings.gradle` | + `include ':sinkon-native'` with `projectDir = file('sinkon-native')` |
| new files | the three specs, the surface bridge, `SinkonRenderSpecs.kt`, `strings_sinkon.xml` |

Nothing else in the launcher is touched: `compatibleDevice()` keeps Sinkon out of the
picker on devices that cannot run it, and every bridge call is a no-op unless
`LauncherPreferences.PREF_RENDERER` is one of the three Sinkon tags.

## Apply

```sh
git clone https://github.com/MojoLauncher/MojoLauncher.git
cd MojoLauncher && git checkout v3_openjdk
git apply /path/to/Sinkon/launcher-fork/patches/0001-register-sinkon-renderers.patch

# the native module becomes :sinkon-native
cp -r /path/to/Sinkon/android/app ./sinkon-native
./scripts/build-android.sh          # from the Sinkon repo; or copy the .so files by hand
./gradlew assembleDebug             # NOT RUN yet — see docs/KNOWN_ISSUES.md SINKON-002
```

The module's `namespace` is `org.sinkon.launcher`, so the JNI symbol prefix
`Java_org_sinkon_launcher_Sinkon_` matches regardless of the fork's application id.

## Lifecycle contract

```
GameView.onSurfaceAvailable  → SinkonSurfaceBridge.onSurfaceAvailable(ctx, surface, w, h)
                                  └─ new SinkonSession(mcVersion, backendHint)
                                     SinkonRenderLoop.start(surface, w, h)
                                        └─ (render thread) Sinkon.nativeInit(...) → jlong
GameView.onSurfaceResized    → …onSurfaceResized(w, h) → Sinkon.nativeResize
(frame, every vsync)         → Sinkon.nativeSetCamera + Sinkon.nativeRenderFrame
GameView.onSurfaceDestroyed  → …onSurfaceDestroyed()   → Sinkon.nativeDestroy
```

Set `SinkonSurfaceBridge.minecraftVersion` from the launcher's launch path before the
first surface arrives (SINKON-007); it selects the version profile
(Legacy/Transition/Modern/Current).

## Integration risks (read before shipping)

1. **One presenter per window** (SINKON-005). Sinkon presents to the game's
   `ANativeWindow`; the launcher's own EGL/GLFW path still receives the same surface
   through `Platform.updateSurface`. The two must not present simultaneously — the
   fork has to route the surface exclusively to Sinkon when a Sinkon renderer is
   selected, or composite Sinkon through an `AHardwareBuffer`.
2. **No world-data hook** (SINKON-006). Nothing in the stock launcher calls
   `nativeBakeSection`, so a fresh fork will present an empty (clear-colour) world
   until the chunk bridge lands. `SinkonSession.pushSection` is the prepared entry
   point.
3. **Shaderpacks are diagnostics-only** (SINKON-004): packs are parsed, translated
   and validated per backend, but the deferred/composite chain is not executed yet.
4. **Badges** (SINKON-008): `RenderSpec.badge()` exists and the drawables are
   shipped, but the stock preference list only renders names.

## Re-generating the patch

```sh
git clone --depth 1 -b v3_openjdk https://github.com/MojoLauncher/MojoLauncher.git /tmp/mojo
# copy the new files in, apply the edits listed in the table above, then:
cd /tmp/mojo && git add -A -N && git diff > Sinkon/launcher-fork/patches/0001-register-sinkon-renderers.patch
```
