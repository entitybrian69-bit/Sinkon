package org.sinkon.sample

import kotlin.math.tan

/**
 * Minimal matrix helpers for the harness.
 *
 * Sinkon expects **column-major** 4×4 matrices (the same convention as GLM and
 * `[[f32; 4]; 4]` in Rust, where `m[3]` is the translation column), and it composes
 * `u_mvp = proj * view` itself — the sample only supplies the two matrices.
 */
object Camera {

    /** Right-handed perspective projection, column-major. */
    fun perspective(fovYDegrees: Float, aspect: Float, near: Float, far: Float): FloatArray {
        val f = 1.0f / tan(Math.toRadians(fovYDegrees.toDouble() / 2.0)).toFloat()
        val range = 1.0f / (near - far)
        return FloatArray(16) { 0.0f }.also { m ->
            m[0] = f / aspect
            m[5] = f
            m[10] = (near + far) * range
            m[11] = -1.0f
            m[14] = 2.0f * near * far * range
        }
    }

    /** Right-handed look-at view matrix, column-major. */
    fun lookAt(
        eyeX: Float, eyeY: Float, eyeZ: Float,
        centerX: Float, centerY: Float, centerZ: Float,
        upX: Float, upY: Float, upZ: Float,
    ): FloatArray {
        var fx = centerX - eyeX
        var fy = centerY - eyeY
        var fz = centerZ - eyeZ
        val fLength = kotlin.math.sqrt(fx * fx + fy * fy + fz * fz).takeIf { it > 1e-6f } ?: 1.0f
        fx /= fLength; fy /= fLength; fz /= fLength

        // right = normalize(forward × up)
        var sx = fy * upZ - fz * upY
        var sy = fz * upX - fx * upZ
        var sz = fx * upY - fy * upX
        val sLength = kotlin.math.sqrt(sx * sx + sy * sy + sz * sz).takeIf { it > 1e-6f } ?: 1.0f
        sx /= sLength; sy /= sLength; sz /= sLength

        // up' = right × forward
        val ux = sy * fz - sz * fy
        val uy = sz * fx - sx * fz
        val uz = sx * fy - sy * fx

        return floatArrayOf(
            sx, ux, -fx, 0.0f,
            sy, uy, -fy, 0.0f,
            sz, uz, -fz, 0.0f,
            -(sx * eyeX + sy * eyeY + sz * eyeZ),
            -(ux * eyeX + uy * eyeY + uz * eyeZ),
            (fx * eyeX + fy * eyeY + fz * eyeZ),
            1.0f,
        )
    }
}
