package org.sinkon.launcher

import android.app.ActivityManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.util.Log

/**
 * Device capability probes used by the renderer specs' `compatibleDevice()`.
 *
 * These are deliberately cheap and allocation-free after the first call: the
 * launcher asks them while building the renderer list, and Sinkon asks them again
 * in `nativeInit` (via the native backend selection) to decide the Auto order.
 */
object SinkonProbe {

    /** Vulkan 1.1 is the minimum Sinkon can use (`VK_API_VERSION_1_1` = 0x00401000). */
    private const val VULKAN_1_1 = 0x0040_1000

    /** GLES 3.0 in `reqGlEsVersion` encoding. */
    private const val GLES_3_0 = 0x0003_0000

    /** GLES 3.1 in `reqGlEsVersion` encoding. */
    private const val GLES_3_1 = 0x0003_0001

    /** True when the device advertises a Vulkan 1.1+ driver. */
    fun hasVulkan(context: Context): Boolean = try {
        val manager = context.packageManager
        manager.hasSystemFeature(PackageManager.FEATURE_VULKAN_HARDWARE_VERSION, VULKAN_1_1) &&
            manager.hasSystemFeature(PackageManager.FEATURE_VULKAN_HARDWARE_LEVEL, 1)
    } catch (error: Throwable) {
        Log.w(Sinkon.TAG, "Vulkan probe failed", error)
        false
    }

    /**
     * The device's advertised GLES version (`0x00030000` for 3.0), or `0` when
     * unknown.
     *
     * `ActivityManager.deviceConfigurationInfo` is deprecated from API 31 but is
     * still the only synchronous, allocation-free source of this value; the
     * native side re-checks the real version after creating the EGL context.
     */
    @Suppress("DEPRECATION")
    fun glesVersion(context: Context): Int = try {
        val manager = context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager
        manager?.deviceConfigurationInfo?.reqGlEsVersion ?: 0
    } catch (error: Throwable) {
        Log.w(Sinkon.TAG, "GLES probe failed", error)
        0
    }

    /** GLES 3.0 or newer: the `wgpu::Backends::GL` path is usable. */
    fun hasGles3(context: Context): Boolean = glesVersion(context) >= GLES_3_0

    /** GLES 3.1: required for compute-based culling and SSBOs on the GL path. */
    fun hasGles31(context: Context): Boolean = glesVersion(context) >= GLES_3_1

    /**
     * GLES 2.0/2.1 only, which is the only case where the `glow` legacy path is
     * allowed to take over — see `docs/BACKEND_MATRIX.md`.
     */
    fun hasLegacyGlesOnly(context: Context): Boolean {
        val version = glesVersion(context)
        return version in 0x0002_0000 until GLES_3_0
    }

    /** One-line summary for launcher logs and bug reports. */
    fun describe(context: Context): String {
        val gles = glesVersion(context)
        val glesText = when {
            gles == 0 -> "unknown"
            else -> "${(gles shr 16) and 0xFFFF}.${gles and 0xFFFF}"
        }
        val soc = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            Build.SOC_MODEL.ifEmpty { Build.HARDWARE }
        } else {
            Build.HARDWARE
        }
        return "SoC=$soc" +
            " vulkan=${hasVulkan(context)} gles=$glesText" +
            " sdk=${Build.VERSION.SDK_INT} abi=${Build.SUPPORTED_ABIS.firstOrNull() ?: "?"}"
    }
}
