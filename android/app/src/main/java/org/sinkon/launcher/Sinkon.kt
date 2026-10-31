package org.sinkon.launcher

import android.view.Surface
import java.nio.ByteBuffer
import java.nio.FloatBuffer

/**
 * Kotlin bindings for `libsinkon.so`.
 *
 * Every function here maps 1:1 onto a `Java_org_sinkon_launcher_Sinkon_*` export
 * in `src/jni_bridge.rs`. The class name and package are therefore part of the
 * ABI: renaming either breaks the native lookup.
 *
 * ### Rules for callers
 *
 * * [nativeInit] and [nativeRenderFrame] must run on the same thread — the
 *   launcher's render thread. Sinkon's legacy GLES 2.0 path enforces this.
 * * All buffers handed to the native side must be **direct** (`allocateDirect`);
 *   heap buffers are rejected by the native code and logged, never copied.
 * * [nativeDestroy] must be called exactly once per handle. A stale handle is
 *   detected and logged instead of dereferenced.
 */
object Sinkon {

    /** Value accepted by `backendHint` and [nativeSwitchBackend]. */
    const val HINT_AUTO = 0
    /** Force `wgpu::Backends::VULKAN`. */
    const val HINT_VULKAN = 1
    /** Force `wgpu::Backends::GL` (GLES 3.0/3.1). */
    const val HINT_GLES3 = 2
    /** Force the `glow` GLES 2.0/2.1 legacy path. */
    const val HINT_GLES2_LEGACY = 3

    /** Library name without the `lib` prefix or `.so` suffix. */
    const val LIBRARY = "sinkon"

    /** Log tag used by both the Kotlin side and the native logger. */
    const val TAG = "Sinkon"

    private val loadResult: Result<Unit> = runCatching { System.loadLibrary(LIBRARY) }

    /**
     * Whether `libsinkon.so` could be loaded.
     *
     * Renderer specs must call this from `compatibleDevice()`; calling any
     * `native*` function when this is `false` throws [UnsatisfiedLinkError].
     */
    fun isAvailable(): Boolean = loadResult.isSuccess

    /** The load failure, for logging in `compatibleDevice()`/`setupRenderer()`. */
    fun loadFailure(): Throwable? = loadResult.exceptionOrNull()

    // ---------------------------------------------------------------- lifecycle

    external fun nativeInit(
        surface: Surface,
        width: Int,
        height: Int,
        mcVersion: String,
        backendHint: Int,
    ): Long

    external fun nativeResize(ctx: Long, width: Int, height: Int)

    external fun nativeRenderFrame(ctx: Long)

    external fun nativeDestroy(ctx: Long)

    // ------------------------------------------------------------------- data in

    /**
     * Queue a 16×16×16 section for meshing.
     *
     * @param vbo 4096 block ids, one byte per block, direct buffer.
     * @param ibo optional palette/state buffer; may be an empty direct buffer.
     */
    external fun nativeBakeSection(
        ctx: Long,
        x: Int,
        y: Int,
        z: Int,
        vbo: ByteBuffer,
        ibo: ByteBuffer,
        formatId: Int,
    )

    /** Column-major view and projection matrices, 16 floats each, direct buffers. */
    external fun nativeSetCamera(ctx: Long, view: FloatBuffer, proj: FloatBuffer)

    /** RGBA8 texture upload; `pixels` is a direct buffer holding `w * h * 4` bytes. */
    external fun nativeUploadTexture(
        ctx: Long,
        texId: Int,
        width: Int,
        height: Int,
        pixels: ByteBuffer,
    )

    /** Point the shader layer at a shaderpack directory (`null` clears it). */
    external fun nativeSetShaderpack(ctx: Long, path: String)

    /** Recompile everything; returns `0` on success, `1` on partial failure, `-1` on error. */
    external fun nativeReloadShaders(ctx: Long): Int

    // ------------------------------------------------------------------- reports

    /** Capability JSON (see `docs/JNI.md` for the schema). `null` on failure. */
    external fun nativeGetCapabilityReport(ctx: Long): String?

    /** Frame-time histogram + counters as JSON. `null` on failure. */
    external fun nativeGetPerfStats(ctx: Long): String?

    /** Attempt a live backend switch; `0` on success, non-zero on failure. */
    external fun nativeSwitchBackend(ctx: Long, backendHint: Int): Int
}

/**
 * Reusable direct float buffers for [Sinkon.nativeSetCamera].
 *
 * `allocateDirect` is expensive and the matrices are pushed every frame, so the
 * launcher keeps one instance per session and only writes new values into it.
 * This mirrors the native contract: `nativeSetCamera` reads 16 floats from each
 * buffer via `GetDirectBufferAddress` and copies nothing else.
 */
class DirectMatrices {
    /** 4×4 column-major view matrix, direct. */
    val view: FloatBuffer = ByteBuffer.allocateDirect(MATRIX_BYTES).asFloatBuffer()

    /** 4×4 column-major projection matrix, direct. */
    val proj: FloatBuffer = ByteBuffer.allocateDirect(MATRIX_BYTES).asFloatBuffer()

    /** Write both matrices. Accepts 16-element column-major arrays. */
    fun set(viewMatrix: FloatArray, projectionMatrix: FloatArray) {
        require(viewMatrix.size == MATRIX_FLOATS && projectionMatrix.size == MATRIX_FLOATS) {
            "expected 16 floats per matrix"
        }
        view.clear()
        view.put(viewMatrix)
        view.flip()
        proj.clear()
        proj.put(projectionMatrix)
        proj.flip()
    }

    private companion object {
        const val MATRIX_FLOATS = 16
        const val MATRIX_BYTES = MATRIX_FLOATS * 4
    }
}
