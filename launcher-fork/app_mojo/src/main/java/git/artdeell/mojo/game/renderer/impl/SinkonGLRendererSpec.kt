package git.artdeell.mojo.game.renderer.impl

import android.content.Context
import git.artdeell.mojo.R
import git.artdeell.mojo.game.renderer.def.Renderers
import org.sinkon.launcher.Sinkon
import org.sinkon.launcher.SinkonProbe

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
