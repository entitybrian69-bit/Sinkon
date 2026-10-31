package git.artdeell.mojo.game.renderer.impl

import android.content.Context
import android.util.Log
import android.view.Surface
import git.artdeell.mojo.prefs.LauncherPreferences
import org.sinkon.launcher.Sinkon
import org.sinkon.launcher.SinkonProbe
import org.sinkon.launcher.SinkonRenderLoop
import org.sinkon.launcher.SinkonSession

/**
 * Bridges the launcher's surface lifecycle to a Sinkon session.
 *
 * Patch 0001 adds three one-line calls to `GameView`:
 *
 * ```java
 * onSurfaceAvailable(Surface surface)  -> SinkonSurfaceBridge.onSurfaceAvailable(surface, getWidth(), getHeight());
 * onSurfaceResized()                   -> SinkonSurfaceBridge.onSurfaceResized(getWidth(), getHeight());
 * onSurfaceDestroyed()                 -> SinkonSurfaceBridge.onSurfaceDestroyed();
 * ```
 *
 * The calls are no-ops unless a Sinkon renderer is the selected `RenderSpec`
 * (`LauncherPreferences.PREF_RENDERER`), so the stock GLES/Zink/Mesa paths are
 * untouched.
 *
 * Set [minecraftVersion] from the launcher's launch path before the first surface
 * arrives; it decides the version profile (Legacy/Transition/Modern/Current).
 */
object SinkonSurfaceBridge {

    /** MC version string passed to `nativeInit`. */
    @Volatile
    var minecraftVersion: String = "1.20.4"

    /** Last known surface size, updated by the `GameView` hooks. */
    @Volatile
    var surfaceWidth: Int = 0
        private set

    @Volatile
    var surfaceHeight: Int = 0
        private set

    private var loop: SinkonRenderLoop? = null
    private var session: SinkonSession? = null
    private var appContext: Context? = null

    /** The active session, or `null` when Sinkon is not presenting. */
    fun session(): SinkonSession? = session

    /** True when the currently selected renderer is one of the Sinkon specs. */
    fun isSelected(): Boolean = when (LauncherPreferences.PREF_RENDERER) {
        Renderers.SINKON_VULKAN_RENDERER,
        Renderers.SINKON_GL_RENDERER,
        Renderers.SINKON_AUTO_RENDERER -> true
        else -> false
    }

    /** `GameView.onSurfaceAvailable` hook. */
    fun onSurfaceAvailable(context: Context, surface: Surface, width: Int, height: Int) {
        if (!isSelected()) return
        appContext = context.applicationContext ?: context
        surfaceWidth = width
        surfaceHeight = height
        if (loop != null) {
            Log.w(Sinkon.TAG, "surface available while a loop is running; restarting")
            onSurfaceDestroyed()
        }
        val spec = selectedSpec() ?: return
        val session = spec.createSession(minecraftVersion)
        val loop = SinkonRenderLoop(session)
        this.session = session
        this.loop = loop
        val probe = appContext?.let { SinkonProbe.describe(it) } ?: "probe unavailable"
        Log.i(Sinkon.TAG, "attaching ${spec.name()} to ${width}x${height} ($probe)")
        if (!loop.start(surface, width, height)) {
            Log.e(Sinkon.TAG, "Sinkon render loop failed to start; falling back to the launcher path")
            this.session = null
            this.loop = null
        }
    }

    /** `GameView.onSurfaceResized` hook. */
    fun onSurfaceResized(width: Int, height: Int) {
        if (!isSelected()) return
        if (width == surfaceWidth && height == surfaceHeight) return
        surfaceWidth = width
        surfaceHeight = height
        loop?.resize(width, height)
    }

    /** `GameView.onSurfaceDestroyed` hook. */
    fun onSurfaceDestroyed() {
        loop?.stop()
        loop = null
        session?.detach()
        session = null
        appContext = null
    }

    private fun selectedSpec(): SinkonRenderSpec? = when (LauncherPreferences.PREF_RENDERER) {
        Renderers.SINKON_VULKAN_RENDERER -> SinkonVulkanRendererSpec()
        Renderers.SINKON_GL_RENDERER -> SinkonGLRendererSpec()
        Renderers.SINKON_AUTO_RENDERER -> SinkonAutoRendererSpec()
        else -> null
    }
}
