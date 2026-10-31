package git.artdeell.mojo.game.renderer.impl

import android.content.Context
import android.util.Log
import git.artdeell.mojo.game.renderer.RenderSpec
import org.sinkon.launcher.Sinkon
import org.sinkon.launcher.SinkonProbe
import org.sinkon.launcher.SinkonSession

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
