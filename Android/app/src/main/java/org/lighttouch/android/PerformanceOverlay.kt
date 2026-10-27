package org.lighttouch.android

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.util.Locale

private fun f1(value: Float) = String.format(Locale.ROOT, "%.1f", value)
private fun f0(value: Float) = String.format(Locale.ROOT, "%.0f", value)
private fun bytesPerSecond(bytes: Double) =
    if (bytes >= 1e6) String.format(Locale.ROOT, "%.1f MB/s", bytes / 1e6) else String.format(Locale.ROOT, "%.0f KB/s", bytes / 1e3)
private fun count(perSecond: Double) =
    if (perSecond >= 1000) String.format(Locale.ROOT, "%.1fk", perSecond / 1000) else String.format(Locale.ROOT, "%.0f", perSecond)

/**
 * MangoHud-style readout over the guest screen. [level] picks how much: FPS alone; FPS, frame
 * time and a graph; or everything the engine can report, including detailed timing.
 */
@Composable
internal fun PerformanceOverlay(snapshot: PerfSnapshot, level: OverlayLevel, graph: OverlayGraph, opacity: Float,
                                detailedTiming: Boolean, modifier: Modifier = Modifier, scale: Float = 1f) {
    if (level == OverlayLevel.OFF) return
    val text = MonoStyle.copy(fontSize = (11 * scale).sp, lineHeight = (14 * scale).sp, color = Color.White)
    Column(modifier.testTag("performance_overlay")
        .background(Color.Black.copy(alpha = opacity), RoundedCornerShape((6 * scale).dp))
        .padding(horizontal = (6 * scale).dp, vertical = (4 * scale).dp)) {
        @Composable fun row(label: String, color: Color, value: String) = Text(buildAnnotatedString {
            withStyle(SpanStyle(color = color, fontWeight = FontWeight.Bold)) { append(label.padEnd(5)) }
            append(value)
        }, style = text, maxLines = 1)
        val fps = if (snapshot.fps > 0) f0(snapshot.fps) else if (snapshot.idle) "idle" else "–"
        when (level) {
            OverlayLevel.FPS -> Text(buildAnnotatedString {
                withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(fps) }
                withStyle(SpanStyle(color = LtColors.OverlayFps, fontSize = (9 * scale).sp)) { append(" FPS") }
            }, style = text.copy(fontSize = (13 * scale).sp))
            OverlayLevel.COMPACT -> {
                row("FPS", LtColors.OverlayFps, "${fps.padStart(3)}  ${f1(snapshot.frameMs)} ms")
                FrameGraph(snapshot.intervals, graph, Modifier.padding(top = 2.dp).size((132 * scale).dp, (26 * scale).dp))
            }
            else -> {
                row("FPS", LtColors.OverlayFps, "${fps.padStart(3)}  ${f1(snapshot.frameMs)} ms")
                row("1%", LtColors.OverlayFps, if (snapshot.lowFps > 0) "${f0(snapshot.lowFps).padStart(3)}  max ${f1(snapshot.maxFrameMs)} ms" else "  –")
                FrameGraph(snapshot.intervals, graph, Modifier.padding(vertical = 2.dp).size((168 * scale).dp, (34 * scale).dp))
                row("CPU", LtColors.OverlayCpu, buildString {
                    append("guest ${snapshot.vcpuPercent?.let { f0(it) + "%" } ?: "–"}")
                    if (snapshot.vcpuCore != null) append(" · cpu${snapshot.vcpuCore}")
                    snapshot.vcpuMHz?.let { append(" ${f1(it / 1000f)}GHz") }
                })
                row("", LtColors.OverlayCpu, "engine ${snapshot.workerPercent?.let { f0(it) + "%" } ?: "–"}")
                if (snapshot.detailedAvailable) {
                    row("GPU", LtColors.OverlayGpu, "GL ${f1(snapshot.glMsPerFrame ?: 0f)} ms/frame")
                    row("LOCK", LtColors.OverlayGpu, "BQL ${f1(snapshot.bqlMsPerFrame ?: 0f)} ms/frame")
                    row("RUN", LtColors.OverlayCpu, "vCPU ${f0(snapshot.runPercent ?: 0f)}% of wall")
                } else row("GPU", LtColors.OverlayGpu, when {
                    !detailedTiming -> "detailed timing off"
                    snapshot.detailedError != null -> "timing unavailable"
                    else -> "waiting for frames…"
                })
                row("PRES", LtColors.OverlayLabel, buildString {
                    append(snapshot.presentMs?.let { f1(it) + " ms" } ?: "–")
                    snapshot.latencyMs?.let { append(" · lat ${f1(it)} ms") }
                })
                snapshot.pssMiB?.let { row("MEM", LtColors.OverlayMem, "${f0(it)} MiB") }
                // The guest's NAND traffic as its flash controller issues it, and the host time spent serving it.
                snapshot.storage?.let { io ->
                    row("DISK", LtColors.OverlayDisk, "read  ${bytesPerSecond(io.readBytes)} · ${count(io.readOps)} IO/s")
                    row("", LtColors.OverlayDisk, "write ${bytesPerSecond(io.writeBytes)} · ${count(io.writeOps)} IO/s")
                    row("", LtColors.OverlayDisk,
                        "host  ${f0(io.busyPercent.toFloat())}% · ${f0(io.usPerOp.toFloat())} µs/IO · ${count(io.eraseOps)} erase/s")
                }
            }
        }
    }
}

/** Frame time (0–50 ms, guides at 16.7 and 33.3) or frame rate (0–60) over the last few seconds. */
@Composable
internal fun FrameGraph(intervals: FloatArray, graph: OverlayGraph, modifier: Modifier) {
    Canvas(modifier) {
        val guide = Color.White.copy(alpha = 0.18f)
        val frameTime = graph == OverlayGraph.FRAME_TIME
        val top = if (frameTime) 50f else 60f
        fun y(v: Float) = size.height - (v.coerceIn(0f, top) / top) * size.height
        for (g in if (frameTime) listOf(16.7f, 33.3f) else listOf(30f, 60f)) drawLine(guide, Offset(0f, y(g)), Offset(size.width, y(g)), 1f)
        if (intervals.size < 2) return@Canvas
        val shown = intervals.takeLast(240)
        // A static screen's gap is drawn as a break in the line, not as a spike.
        val step = size.width / (shown.size - 1).coerceAtLeast(1)
        val line = Path()
        var pen = false
        shown.forEachIndexed { i, ms ->
            if (ms >= IDLE_GAP_MS) { pen = false; return@forEachIndexed }
            val v = if (frameTime) ms else if (ms > 0) 1000f / ms else 0f
            if (!pen) line.moveTo(i * step, y(v)) else line.lineTo(i * step, y(v))
            pen = true
        }
        drawPath(line, LtColors.OverlayGpu, style = Stroke(1.5f))
        // A spike over 50 ms is marked at the top so a hitch never hides off-scale.
        if (frameTime) shown.forEachIndexed { i, ms -> if (ms > 50f && ms < IDLE_GAP_MS) drawCircle(LtColors.Danger, 2.dp.toPx(), Offset(i * step, 2.dp.toPx())) }
    }
}

internal fun Alignment.Companion.of(corner: OverlayCorner) = when (corner) {
    OverlayCorner.TOP_LEFT -> TopStart; OverlayCorner.TOP_RIGHT -> TopEnd
    OverlayCorner.BOTTOM_LEFT -> BottomStart; OverlayCorner.BOTTOM_RIGHT -> BottomEnd
}
