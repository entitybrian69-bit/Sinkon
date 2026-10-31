package org.sinkon.sample

import android.app.Activity
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.Gravity
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.widget.FrameLayout
import android.widget.TextView
import org.sinkon.launcher.Sinkon
import org.sinkon.launcher.SinkonProbe
import org.sinkon.launcher.SinkonRenderLoop
import org.sinkon.launcher.SinkonSession
import java.nio.ByteBuffer
import kotlin.math.cos
import kotlin.math.sin

/**
 * Smallest possible driver for `libsinkon.so`.
 *
 * It exists so the renderer can be smoke-tested on a device without a launcher
 * fork: it builds a handful of synthetic 16³ sections, orbits a camera around them
 * and prints the capability/perf JSON into an on-screen HUD.
 *
 * Backend selection: the `backend` intent extra
 * (`adb shell am start -n org.sinkon.sample/.MainActivity --ei backend 1`) —
 * `0` auto, `1` Vulkan, `2` GLES3, `3` GLES2 legacy.
 *
 * This is a **debug harness**, not the product: it draws Sinkon's built-in terrain
 * shader, not Minecraft, and the shaders/world data a launcher would push (see
 * `docs/KNOWN_ISSUES.md` SINKON-004/006) are absent.
 */
class MainActivity : Activity(), SurfaceHolder.Callback {

    private lateinit var surfaceView: SurfaceView
    private lateinit var hud: TextView

    private var session: SinkonSession? = null
    private var loop: SinkonRenderLoop? = null
    private var backendHint = Sinkon.HINT_AUTO

    private val mainHandler = Handler(Looper.getMainLooper())
    private var framesSinceReport = 0
    private var frames = 0L
    private var lastReportNanos = 0L

    /** One 4096-byte direct buffer, reused for every section push. */
    private val sectionBuffer: ByteBuffer = ByteBuffer.allocateDirect(SECTION_BLOCKS)
    private var worldUploaded = false
    private val viewMatrix = FloatArray(16)
    private val projMatrix = FloatArray(16)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        backendHint = intent?.getIntExtra("backend", Sinkon.HINT_AUTO) ?: Sinkon.HINT_AUTO
        buildUi()
        Log.i(Sinkon.TAG, "sample: ${SinkonProbe.describe(this)}, hint=$backendHint")
        if (!Sinkon.isAvailable()) {
            hud.text = getString(R.string.sample_library_missing, Sinkon.loadFailure()?.message ?: "?")
        }
    }

    private fun buildUi() {
        val root = FrameLayout(this)
        surfaceView = SurfaceView(this).apply {
            holder.addCallback(this@MainActivity)
        }
        hud = TextView(this).apply {
            setTextColor(0xFFF1F5F9.toInt())
            textSize = 11f
            setShadowLayer(2f, 0f, 1f, 0xFF000000.toInt())
            setPadding(24, 24, 24, 24)
            text = getString(R.string.sample_starting)
        }
        root.addView(
            surfaceView,
            FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.MATCH_PARENT,
            ),
        )
        root.addView(
            hud,
            FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.WRAP_CONTENT,
                FrameLayout.LayoutParams.WRAP_CONTENT,
                Gravity.TOP or Gravity.START,
            ),
        )
        setContentView(root)
    }

    // --- surface lifecycle: Sinkon must see attach → resize* → detach in order ----

    override fun surfaceCreated(holder: SurfaceHolder) {
        if (!Sinkon.isAvailable()) return
        val created = SinkonSession(Versions.MC_VERSION, backendHint)
        val renderLoop = SinkonRenderLoop(created, object : SinkonRenderLoop.FrameListener {
            override fun onPrepareFrame(frameTimeNanos: Long) {
                if (!worldUploaded) {
                    worldUploaded = true
                    pushTestWorld(created)
                }
                pushCamera(created, frameTimeNanos)
            }

            override fun onFrameRendered(frameTimeNanos: Long) {
                frames++
                framesSinceReport++
                if (framesSinceReport >= REPORT_INTERVAL_FRAMES) {
                    framesSinceReport = 0
                    publishReport(created)
                }
            }
        })
        session = created
        loop = renderLoop
        if (!renderLoop.start(holder.surface, surfaceView.width, surfaceView.height)) {
            hud.text = getString(R.string.sample_attach_failed)
            created.detach()
            session = null
            loop = null
        }
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        loop?.resize(width, height)
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        loop?.stop()
        loop = null
        session = null
        worldUploaded = false
    }

    override fun onDestroy() {
        loop?.stop()
        session?.detach()
        loop = null
        session = null
        super.onDestroy()
    }

    // --- world + camera ----------------------------------------------------------

    /**
     * Three by three sections of a checkerboard slab at y = 0. Enough faces to see
     * that meshing, pooling, culling and the pipeline all work end to end.
     */
    private fun pushTestWorld(target: SinkonSession) {
        for (sectionX in 0 until 3) {
            for (sectionZ in 0 until 3) {
                sectionBuffer.clear()
                for (y in 0 until 16) {
                    for (z in 0 until 16) {
                        for (x in 0 until 16) {
                            val worldX = sectionX * 16 + x
                            val worldZ = sectionZ * 16 + z
                            val solid = y < 3 && (worldX + worldZ) % 5 != 0
                            sectionBuffer.put(if (solid) (1 + (worldX + worldZ) % 7).toByte() else 0)
                        }
                    }
                }
                sectionBuffer.flip()
                target.pushSection(sectionX, 0, sectionZ, sectionBuffer, FORMAT_PACKED8)
            }
        }
        Log.i(Sinkon.TAG, "sample: pushed 9 synthetic sections")
    }

    private fun pushCamera(target: SinkonSession, frameTimeNanos: Long) {
        val seconds = frameTimeNanos / 1_000_000_000.0
        val radius = 42.0f
        val eyeX = 24.0f + radius * cos(seconds * 0.35).toFloat()
        val eyeY = 26.0f
        val eyeZ = 24.0f + radius * sin(seconds * 0.35).toFloat()
        Camera.lookAt(eyeX, eyeY, eyeZ, 24.0f, 1.5f, 24.0f, 0.0f, 1.0f, 0.0f).copyInto(viewMatrix)
        val aspect = if (surfaceView.height == 0) {
            1.0f
        } else {
            surfaceView.width.toFloat() / surfaceView.height.toFloat()
        }
        Camera.perspective(65.0f, aspect, 0.1f, 512.0f).copyInto(projMatrix)
        target.setCamera(viewMatrix, projMatrix)
    }

    /** Runs on a background thread so the render thread never formats strings. */
    private fun publishReport(target: SinkonSession) {
        val now = System.nanoTime()
        val elapsedSeconds = if (lastReportNanos == 0L) 0.0 else (now - lastReportNanos) / 1e9
        lastReportNanos = now
        val fps = if (elapsedSeconds > 0.0) (REPORT_INTERVAL_FRAMES / elapsedSeconds).toInt() else 0
        mainHandler.post {
            val capability = target.capabilityReport() ?: "{}"
            val perf = target.perfStats() ?: "{}"
            hud.text = buildString {
                append("Sinkon sample  •  ").append(fps).append(" fps  •  frames ").append(frames)
                append('\n').append(capability.take(HUD_JSON_CHARS))
                append('\n').append(perf.take(HUD_JSON_CHARS))
            }
        }
    }

    private companion object {
        const val SECTION_BLOCKS = 16 * 16 * 16

        /** `formatId` 8 = 24-byte layout (`MeshFormat::Packed8`). */
        const val FORMAT_PACKED8 = 8
        const val REPORT_INTERVAL_FRAMES = 60
        const val HUD_JSON_CHARS = 1_200
    }

    /** Keeps the version string in one place; drives the version profile. */
    private object Versions {
        const val MC_VERSION = "1.20.4"
    }
}
