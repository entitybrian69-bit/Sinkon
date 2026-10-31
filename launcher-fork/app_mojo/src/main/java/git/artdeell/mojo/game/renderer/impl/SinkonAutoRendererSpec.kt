package git.artdeell.mojo.game.renderer.impl

import android.content.Context
import git.artdeell.mojo.R
import git.artdeell.mojo.game.renderer.def.Renderers
import org.sinkon.launcher.Sinkon
import org.sinkon.launcher.SinkonProbe

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
