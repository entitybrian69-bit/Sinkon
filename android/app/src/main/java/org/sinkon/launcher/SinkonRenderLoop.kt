package org.sinkon.launcher

import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.Process
import android.util.Log
import android.view.Choreographer
import android.view.Surface

/**
 * Standalone render loop for Sinkon.
 *
 * A launcher fork normally drives [SinkonSession] from its own render thread. This
 * helper exists for two cases:
 *
 * 1. integration testing (`adb shell am start …` with the Sinkon sample activity),
 * 2. renderers that want Sinkon to own its frame pacing.
 *
 * It owns a dedicated thread and paces frames with [Choreographer], so every
 * `native*` call happens on the same thread as `nativeInit`.
 */
class SinkonRenderLoop(
    private val session: SinkonSession,
    private val frameListener: FrameListener? = null,
) {

    /** Optional hook so the launcher can push camera/section data each frame. */
    interface FrameListener {
        /** Called on the render thread immediately before the frame is drawn. */
        fun onPrepareFrame(frameTimeNanos: Long)

        /** Called on the render thread after `nativeRenderFrame` returns. */
        fun onFrameRendered(frameTimeNanos: Long) {}
    }

    private var thread: HandlerThread? = null
    private var handler: Handler? = null
    @Volatile private var running = false
    private var pendingSurface: Surface? = null
    private var pendingWidth = 0
    private var pendingHeight = 0

    /** Start the loop; `nativeInit` happens on the new thread. */
    fun start(surface: Surface, width: Int, height: Int): Boolean {
        stop()
        pendingSurface = surface
        pendingWidth = width
        pendingHeight = height
        val thread = HandlerThread("SinkonRender", Process.THREAD_PRIORITY_DISPLAY).also { it.start() }
        this.thread = thread
        val handler = Handler(thread.looper)
        this.handler = handler
        running = true
        handler.post {
            val surface = pendingSurface
            if (surface == null || !session.attach(surface, pendingWidth, pendingHeight)) {
                Log.e(Sinkon.TAG, "SinkonRenderLoop: attach failed")
                running = false
                return@post
            }
            scheduleFrame()
        }
        return true
    }

    /** Resize the surface on the render thread. */
    fun resize(width: Int, height: Int) {
        handler?.post { session.resize(width, height) }
    }

    /** Stop the loop and destroy the native renderer on the render thread. */
    fun stop() {
        running = false
        val handler = handler
        val thread = thread
        this.handler = null
        this.thread = null
        if (handler != null) {
            handler.post { session.detach() }
        }
        thread?.quitSafely()
        try {
            thread?.join(500)
        } catch (interrupted: InterruptedException) {
            Thread.currentThread().interrupt()
        }
    }

    private fun scheduleFrame() {
        if (!running) return
        Choreographer.getInstance().postFrameCallback(frameCallback)
    }

    private val frameCallback = object : Choreographer.FrameCallback {
        override fun doFrame(frameStartNanos: Long) {
            if (!running) return
            try {
                frameListener?.onPrepareFrame(frameStartNanos)
                session.renderFrame()
                frameListener?.onFrameRendered(frameStartNanos)
            } catch (error: Throwable) {
                Log.e(Sinkon.TAG, "SinkonRenderLoop: frame failed", error)
                running = false
                return
            }
            scheduleFrame()
        }
    }

    /** True while frames are being produced. */
    val isRunning: Boolean
        get() = running && Looper.myLooper() != null
}
