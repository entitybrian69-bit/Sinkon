package git.artdeell.mojo.game.renderer.impl

import android.content.Context
import android.util.Log
import git.artdeell.mojo.R
import git.artdeell.mojo.game.renderer.RenderSpec
import git.artdeell.mojo.game.renderer.def.Renderers
import org.sinkon.launcher.Sinkon
import org.sinkon.launcher.SinkonProbe
import org.sinkon.launcher.SinkonSession

/**
 * Shared base for the three Sinkon renderers.
 *
 * Sinkon replaces the whole GL/Vulkan presentation stack: it loads `libsinkon.so`,
 * creates its own surface/device from the launcher's `ANativeWindow` and presents
 * with its own backend. Nothing in the launcher's GLES/Zink/Mesa path is used, so
 * [setupRenderer] deliberately does **not** call `MojoExec.prepareEgl()`.
 */
abstract class SinkonRenderSpec(
    /** `backendHint` forwarded to `nativeInit`: 0 auto, 1 Vulkan, 2 GLES3, 3 GLES2 legacy. */
    private val backendHint: Int,
    private val displayNameRes: Int,
    private val badgeRes: Int,
) : RenderSpec {

    /** `backendHint()` added to the launcher's [RenderSpec] by patch 0001. */
    override fun backendHint(): Int = backendHint

    /** Renderer badge drawable added to the launcher's [RenderSpec] by patch 0001. */
    override fun badge(): Int = badgeRes

    final override fun library(): String = "libsinkon.so"

    final override fun displayName(): Int = displayNameRes

    final override fun setupEnvironment(context: Context, envMap: MutableMap<String, String>) {
        // Sinkon does not go through the launcher's GL library, so no LIBGL_* or
        // MESA_* overrides are needed. The hint travels in the environment purely
        // so a bug report's env dump shows what the user selected.
        envMap["SINKON_BACKEND_HINT"] = backendHint.toString()
        Log.i(Sinkon.TAG, "${name()}: ${SinkonProbe.describe(context)}")
    }

    final override fun setupRenderer(): Boolean {
        if (!Sinkon.isAvailable()) {
            Log.e(Sinkon.TAG, "${name()}: libsinkon.so failed to load", Sinkon.loadFailure())
            return false
        }
        Log.i(Sinkon.TAG, "${name()}: ready (hint=$backendHint, library=${Sinkon.LIBRARY})")
        return true
    }

    /** Creates a session; the launcher attaches it to the game surface. */
    fun createSession(mcVersion: String): SinkonSession = SinkonSession(mcVersion, backendHint)
}

/**
 * `Sinkon (Vulkan)` — forces `wgpu::Backends::VULKAN`.
 *
 * Requires a Vulkan 1.1+ driver; `compatibleDevice()` also checks that the device
 * has a GLES context, because Sinkon's texture atlas path still uses EGL for
 * `AHardwareBuffer` interop on some drivers.
 */
class SinkonVulkanRendererSpec : SinkonRenderSpec(
    backendHint = Sinkon.HINT_VULKAN,
    displayNameRes = R.string.sinkon_renderer_vulkan,
    badgeRes = R.drawable.sinkon_badge_vulkan,
) {
    override fun compatibleDevice(context: Context): Boolean =
        Sinkon.isAvailable() && SinkonProbe.hasVulkan(context)

    override fun name(): String = "Sinkon (Vulkan)"

    override fun tag(): String = Renderers.SINKON_VULKAN_RENDERER
}

/**
 * `Sinkon (OpenGL)` — forces `wgpu::Backends::GL` (GLES 3.0/3.1).
 *
 * When the device only has GLES 2.x, the same spec still works: the native
 * selector drops to the `glow` legacy path and reports it in the capability JSON
 * (`degraded=true`), which is why [compatibleDevice] accepts GLES 2.x too.
 */
class SinkonGLRendererSpec : SinkonRenderSpec(
    backendHint = Sinkon.HINT_GLES3,
    displayNameRes = R.string.sinkon_renderer_gl,
    badgeRes = R.drawable.sinkon_badge_gl,
) {
    override fun compatibleDevice(context: Context): Boolean =
        Sinkon.isAvailable() &&
            (SinkonProbe.hasGles3(context) || SinkonProbe.hasLegacyGlesOnly(context))

    override fun name(): String = "Sinkon (OpenGL)"

    override fun tag(): String = Renderers.SINKON_GL_RENDERER
}

/**
 * `Sinkon (Auto)` — `backendHint = 0`.
 *
 * The native selector walks Vulkan → GLES 3.1/3.0 → `glow` legacy → hard fail,
 * logs every rejection and exposes the outcome in `nativeGetCapabilityReport`.
 */
class SinkonAutoRendererSpec : SinkonRenderSpec(
    backendHint = Sinkon.HINT_AUTO,
    displayNameRes = R.string.sinkon_renderer_auto,
    badgeRes = R.drawable.sinkon_badge_auto,
) {
    override fun compatibleDevice(context: Context): Boolean =
        Sinkon.isAvailable() &&
            (SinkonProbe.hasVulkan(context) ||
                SinkonProbe.hasGles3(context) ||
                SinkonProbe.hasLegacyGlesOnly(context))

    override fun name(): String = "Sinkon (Auto)"

    override fun tag(): String = Renderers.SINKON_AUTO_RENDERER
}
