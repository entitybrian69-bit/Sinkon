package git.artdeell.mojo.game.renderer.impl

import android.content.Context
import git.artdeell.mojo.R
import git.artdeell.mojo.game.renderer.def.Renderers
import org.sinkon.launcher.Sinkon
import org.sinkon.launcher.SinkonProbe

/**
 * `Sinkon (Vulkan)` — forces `wgpu::Backends::VULKAN`.
 *
 * Requires a Vulkan 1.1+ driver (`VK_API_VERSION_1_1`), which is what
 * `SinkonProbe.hasVulkan` asks the package manager for.
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
