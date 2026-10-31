package org.sinkon.launcher

import android.view.Surface
import java.nio.ByteBuffer

/**
 * A single Sinkon renderer session (one native handle).
 *
 * The class is **thread-confined**: [attach], [renderFrame], [resize] and
 * [detach] must all be called from the same thread, which is the thread that
 * owns the `ANativeWindow`. In a launcher that is the game's render thread; for
 * standalone tests use [SinkonRenderLoop], which owns such a thread for you.
 *
 * All hot-path calls are allocation-free: the matrices live in a [DirectMatrices]
 * instance created in [attach], and section pushes reuse a single 4096-byte
 * direct buffer (see [sectionBuffer]).
 */
class SinkonSession(
    private val mcVersion: String,
    /** See [Sinkon.HINT_AUTO] … [Sinkon.HINT_GLES2_LEGACY]. */
    val backendHint: Int,
) {

    /** Native handle; `0` when detached. */
    val handle: Long
        get() = nativeHandle

    /** True between a successful [attach] and [detach]. */
    val isAttached: Boolean
        get() = nativeHandle != 0L

    private var nativeHandle = 0L
    private var matrices: DirectMatrices? = null
    private var sectionBuffer: ByteBuffer? = null
    private var emptyBuffer: ByteBuffer? = null
    private var renderThread: Thread? = null

    /**
     * Create the native renderer for [surface].
     *
     * @return `true` when a backend came up. A `false` return means the caller
     *   should fall back to a different `RenderSpec`; the reason is logged by the
     *   native side under the `Sinkon` tag.
     */
    fun attach(surface: Surface, width: Int, height: Int): Boolean {
        if (nativeHandle != 0L) {
            detach()
        }
        val created = Sinkon.nativeInit(surface, width, height, mcVersion, backendHint)
        if (created == 0L) {
            return false
        }
        nativeHandle = created
        matrices = DirectMatrices()
        sectionBuffer = ByteBuffer.allocateDirect(SECTION_BLOCKS)
        emptyBuffer = ByteBuffer.allocateDirect(0)
        renderThread = Thread.currentThread()
        return true
    }

    /** Swapchain/window size change. Target: ≤ 5 µs. */
    fun resize(width: Int, height: Int) {
        if (nativeHandle == 0L) return
        Sinkon.nativeResize(nativeHandle, width, height)
    }

    /** Render and present one frame. Target: ≤ 50 µs of JNI overhead. */
    fun renderFrame() {
        if (nativeHandle == 0L) return
        Sinkon.nativeRenderFrame(nativeHandle)
    }

    /** Push the camera; call once per frame before [renderFrame]. */
    fun setCamera(viewMatrix: FloatArray, projectionMatrix: FloatArray) {
        val buffers = matrices ?: return
        buffers.set(viewMatrix, projectionMatrix)
        Sinkon.nativeSetCamera(nativeHandle, buffers.view, buffers.proj)
    }

    /**
     * Queue a 16³ section. `blocks` must be a direct buffer of [SECTION_BLOCKS]
     * bytes; it is copied into the session's reusable buffer so the caller may
     * recycle its own buffer immediately.
     */
    fun pushSection(x: Int, y: Int, z: Int, blocks: ByteBuffer, formatId: Int, palette: ByteBuffer? = null) {
        if (nativeHandle == 0L) return
        val target = sectionBuffer ?: return
        target.clear()
        target.put(blocks)
        target.flip()
        Sinkon.nativeBakeSection(
            nativeHandle,
            x,
            y,
            z,
            target,
            palette ?: emptyBuffer ?: target,
            formatId,
        )
    }

    /** RGBA8 texture upload, `width * height * 4` bytes in a direct buffer. */
    fun uploadTexture(textureId: Int, width: Int, height: Int, pixels: ByteBuffer) {
        if (nativeHandle == 0L) return
        Sinkon.nativeUploadTexture(nativeHandle, textureId, width, height, pixels)
    }

    /** Attach or clear a shaderpack directory. */
    fun setShaderpack(path: String?) {
        if (nativeHandle == 0L) return
        Sinkon.nativeSetShaderpack(nativeHandle, path ?: "")
    }

    /** `true` when every shader recompiled cleanly. */
    fun reloadShaders(): Boolean = nativeHandle != 0L && Sinkon.nativeReloadShaders(nativeHandle) == 0

    /** Capability JSON, or `null` when detached. */
    fun capabilityReport(): String? = if (nativeHandle == 0L) null else Sinkon.nativeGetCapabilityReport(nativeHandle)

    /** Perf-stats JSON, or `null` when detached. */
    fun perfStats(): String? = if (nativeHandle == 0L) null else Sinkon.nativeGetPerfStats(nativeHandle)

    /** Live backend switch (Auto only). `0` means success. */
    fun switchBackend(hint: Int): Int = if (nativeHandle == 0L) -1 else Sinkon.nativeSwitchBackend(nativeHandle, hint)

    /** Release the native renderer. Safe to call twice. */
    fun detach() {
        val handle = nativeHandle
        nativeHandle = 0L
        matrices = null
        sectionBuffer = null
        emptyBuffer = null
        renderThread = null
        if (handle != 0L) {
            Sinkon.nativeDestroy(handle)
        }
    }

    /** The thread that attached, for assertions in launcher code. */
    fun isOnRenderThread(): Boolean = renderThread == null || Thread.currentThread() === renderThread

    private companion object {
        const val SECTION_BLOCKS = 16 * 16 * 16
    }
}
