package org.lighttouch.inferno

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import org.lighttouch.inferno.ui.Glyphs
import org.lighttouch.inferno.ui.InfernoTheme
import org.lighttouch.inferno.ui.Mono
import org.lighttouch.inferno.ui.Palette
import org.lighttouch.inferno.ui.Panel
import java.io.File
import java.io.RandomAccessFile

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
        setContent { InfernoTheme { Launcher() } }
    }
}

/** Everything the launcher shows, read off the disk in one go. */
private data class Snapshot(
    val requirements: List<VmConfig.Requirement> = emptyList(),
    val rootImage: VmConfig.RootImage? = null,
    val systemInstalled: Boolean = false,
    val running: Boolean = false,
    val engineLog: String = "",
    val consoleLog: String = "",
    val lastExit: String? = null,
) {
    val missing get() = requirements.filterNot { it.present }
    val ready get() = requirements.isNotEmpty() && missing.isEmpty()
}

private fun snapshot(context: Context): Snapshot {
    val config = Prefs.config(context)
    return Snapshot(
        requirements = config.requirements(),
        rootImage = config.rootImage,
        systemInstalled = config.systemInstalled,
        running = Prefs.vmRunning(context),
        engineLog = tail(config.engineLog, 60),
        consoleLog = tail(config.guestConsoleLog, 60),
        lastExit = Prefs.exitMarker(context).takeIf { it.isFile }?.readText()?.trim(),
    )
}

fun tail(file: File, lines: Int, maxBytes: Int = 64 * 1024): String = runCatching {
    RandomAccessFile(file, "r").use { f ->
        val start = (f.length() - maxBytes).coerceAtLeast(0)
        f.seek(start)
        val bytes = ByteArray((f.length() - start).toInt())
        f.readFully(bytes)
        String(bytes, Charsets.UTF_8).lines().dropLastWhile { it.isBlank() }.takeLast(lines).joinToString("\n")
    }
}.getOrDefault("")

/** Unpacks the engine's data files (-L) once per installed version. */
private fun prepareQemuData(context: Context): File {
    val dir = Prefs.qemuData(context)
    val stamp = File(dir, ".version")
    val version = context.packageManager.getPackageInfo(context.packageName, 0).lastUpdateTime.toString()
    if (stamp.isFile && stamp.readText() == version) return dir
    dir.deleteRecursively()
    dir.mkdirs()
    fun copy(asset: String, dest: File) {
        val children = context.assets.list(asset).orEmpty()
        if (children.isEmpty()) {
            runCatching { context.assets.open(asset).use { input -> dest.outputStream().use { input.copyTo(it) } } }
        } else {
            dest.mkdirs()
            children.forEach { copy("$asset/$it", File(dest, it)) }
        }
    }
    context.assets.list("qemu-data").orEmpty().forEach { copy("qemu-data/$it", File(dir, it)) }
    stamp.writeText(version)
    return dir
}

private fun launchVm(context: Context, settings: VmSettings) {
    val config = Prefs.config(context)
    val args = config.arguments(settings, prepareQemuData(context))
    context.startForegroundService(
        Intent(context, VmService::class.java)
            .setAction(VmService.ACTION_START)
            .putExtra(VmService.EXTRA_ARGS, args.toTypedArray())
            .putExtra(VmService.EXTRA_HAPTICS, settings.haptics),
    )
    openVm(context)
}

private fun openVm(context: Context) = context.startActivity(Intent(context, VmActivity::class.java))

@Composable
private fun Launcher() {
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    var snap by remember { mutableStateOf(Snapshot()) }
    var settings by remember { mutableStateOf(Prefs.load(context)) }
    val import by ImportState.progress.collectAsState()
    var confirmBlank by remember { mutableStateOf(false) }
    var refresh by remember { mutableIntStateOf(0) }

    LaunchedEffect(lifecycle, import.running, refresh) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            while (true) {
                snap = withContext(Dispatchers.IO) { snapshot(context) }
                delay(if (snap.running || import.running) 1000 else 3000)
            }
        }
    }

    fun update(s: VmSettings) { settings = s; Prefs.save(context, s) }

    val pickFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        uri?.let { ImportService.start(context, ImportService.KIND_FOLDER, it) }
    }
    val pickZip = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        uri?.let { ImportService.start(context, ImportService.KIND_ZIP, it) }
    }
    val pickFile = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        uri?.let { ImportService.start(context, ImportService.KIND_FILE, it) }
    }

    Box(Modifier.fillMaxSize().background(Palette.background)) {
        Column(
            Modifier
                .fillMaxSize()
                .safeDrawingPadding()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Header()
            Hero(snap, settings)
            LaunchControls(
                snap = snap,
                onLaunch = {
                    if (snap.rootImage != null && !snap.systemInstalled) confirmBlank = true
                    else launchVm(context, settings)
                },
                onOpen = { openVm(context) },
                onStop = { VmService.stop(context) },
            )
            FilesPanel(snap, Prefs.root(context))
            ImportPanel(
                progress = import,
                onFolder = { pickFolder.launch(null) },
                onZip = { pickZip.launch(arrayOf("application/zip", "application/x-zip-compressed", "application/octet-stream")) },
                onFile = { pickFile.launch(arrayOf("*/*")) },
                onCancel = { ImportState.cancelRequested = true },
                onDismiss = { ImportState.clearMessage(); refresh++ },
            )
            SettingsPanel(settings, Prefs.deviceMemoryGb(context), ::update)
            LogsPanel(snap)
            Text(
                "Движок: MakrSas/Inferno (ChefKiss Inferno), AGPL-3.0. Образ iOS в приложение не входит — нужен свой набор файлов.",
                style = MaterialTheme.typography.bodySmall, color = Palette.Muted,
                modifier = Modifier.padding(horizontal = 8.dp, vertical = 8.dp),
            )
        }
    }

    if (confirmBlank) {
        AlertDialog(
            onDismissRequest = { confirmBlank = false },
            title = { Text("Диск выглядит пустым") },
            text = { Text("В образе диска нет ни одного записанного блока — система, похоже, не установлена. Машина будет просто ждать загрузочный диск. Всё равно запустить?") },
            confirmButton = { TextButton(onClick = { confirmBlank = false; launchVm(context, settings) }) { Text("Запустить") } },
            dismissButton = { TextButton(onClick = { confirmBlank = false }) { Text("Отмена") } },
        )
    }
}

@Composable
private fun Header() {
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(top = 8.dp, start = 4.dp)) {
        Box(
            Modifier.size(44.dp).clip(RoundedCornerShape(14.dp)).background(Palette.accent),
            contentAlignment = Alignment.Center,
        ) { PhoneGlyph(Modifier.size(18.dp, 28.dp)) }
        Spacer(Modifier.width(14.dp))
        Column {
            Text("Inferno", style = MaterialTheme.typography.headlineLarge, color = Palette.Text)
            Text("iOS 14 на Android", style = MaterialTheme.typography.bodyMedium, color = Palette.Muted)
        }
    }
}

@Composable
private fun PhoneGlyph(modifier: Modifier) {
    Canvas(modifier) {
        val r = size.width * 0.22f
        drawRoundRect(Color.White, cornerRadius = CornerRadius(r, r), style = Stroke(width = size.width * 0.12f))
        drawRoundRect(Color.White, topLeft = Offset(size.width * 0.32f, size.height * 0.08f),
            size = Size(size.width * 0.36f, size.height * 0.06f), cornerRadius = CornerRadius(r, r))
    }
}

/** The phone illustration with the machine's particulars beside it. */
@Composable
private fun Hero(snap: Snapshot, settings: VmSettings) {
    Panel {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Canvas(Modifier.size(78.dp, 160.dp)) {
                val bezel = size.width * 0.06f
                val outer = CornerRadius(size.width * 0.2f, size.width * 0.2f)
                drawRoundRect(Color(0xFF2A2F45), cornerRadius = outer)
                drawRoundRect(Palette.wallpaper, topLeft = Offset(bezel, bezel),
                    size = Size(size.width - 2 * bezel, size.height - 2 * bezel),
                    cornerRadius = CornerRadius(outer.x - bezel, outer.y - bezel))
                // The notch.
                drawRoundRect(Color(0xFF2A2F45), topLeft = Offset(size.width * 0.28f, bezel),
                    size = Size(size.width * 0.44f, size.height * 0.045f), cornerRadius = CornerRadius(12f, 12f))
                // A few icons on the home screen.
                val cell = (size.width - 2 * bezel) / 4f
                for (row in 0 until 4) for (col in 0 until 4) {
                    drawRoundRect(Color.White.copy(alpha = 0.22f + 0.05f * ((row + col) % 3)),
                        topLeft = Offset(bezel + col * cell + cell * 0.18f, size.height * 0.14f + row * cell * 1.15f),
                        size = Size(cell * 0.64f, cell * 0.64f), cornerRadius = CornerRadius(cell * 0.18f, cell * 0.18f))
                }
                drawRoundRect(Color.White.copy(alpha = 0.18f), topLeft = Offset(bezel * 2, size.height * 0.84f),
                    size = Size(size.width - 4 * bezel, size.height * 0.09f), cornerRadius = CornerRadius(14f, 14f))
            }
            Spacer(Modifier.width(18.dp))
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("iPhone 11", style = MaterialTheme.typography.titleLarge, color = Palette.Text)
                Text("t8030 · iOS 14 · ${settings.cores - 1}+SEP ядра · ${settings.memoryGb} ГБ",
                    style = MaterialTheme.typography.bodySmall, color = Palette.Muted)
                Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    if (BuildConfig.ENGINE_BUNDLED) Chip("движок", Palette.Green) else Chip("нет движка", Palette.Red)
                    when {
                        snap.running -> Chip("работает", Palette.Blue)
                        snap.rootImage == null -> Chip("нет диска", Palette.Red)
                        snap.systemInstalled -> Chip("система есть", Palette.Green)
                        else -> Chip("диск пуст", Palette.Amber)
                    }
                }
                Text("${settings.displayWidth}×${settings.displayHeight} @${settings.displayScale}x",
                    style = MaterialTheme.typography.bodySmall, color = Palette.Muted)
            }
        }
    }
}

@Composable
private fun Chip(text: String, color: Color) {
    Row(
        Modifier.clip(CircleShape).background(color.copy(alpha = 0.14f)).padding(horizontal = 10.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(6.dp).clip(CircleShape).background(color))
        Spacer(Modifier.width(6.dp))
        Text(text, fontSize = 12.sp, color = color, fontWeight = FontWeight.Medium)
    }
}

@Composable
private fun LaunchControls(snap: Snapshot, onLaunch: () -> Unit, onOpen: () -> Unit, onStop: () -> Unit) {
    val blocker = when {
        !BuildConfig.ENGINE_BUNDLED -> "В эту сборку не встроен движок (libqemu-aarch64-softmmu.so)."
        snap.running -> null
        !snap.ready -> "Не хватает файлов: ${snap.missing.size}. Импортируй набор ниже."
        else -> null
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (snap.running) {
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                GradientButton("Открыть экран", Icons.Filled.PlayArrow, Modifier.weight(1f), true, onOpen)
                OutlinedButton(onClick = onStop, modifier = Modifier.height(56.dp), shape = RoundedCornerShape(18.dp)) {
                    Icon(Glyphs.Power, null, Modifier.size(18.dp)); Spacer(Modifier.width(6.dp)); Text("Выключить")
                }
            }
        } else {
            GradientButton("Запустить iOS", Icons.Filled.PlayArrow, Modifier.fillMaxWidth(), blocker == null, onLaunch)
        }
        blocker?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = Palette.Amber, modifier = Modifier.padding(horizontal = 8.dp)) }
    }
}

@Composable
private fun GradientButton(text: String, icon: ImageVector, modifier: Modifier, enabled: Boolean, onClick: () -> Unit) {
    val shape = RoundedCornerShape(18.dp)
    Box(
        modifier
            .height(56.dp)
            .clip(shape)
            .background(if (enabled) Palette.accent else Brush.linearGradient(listOf(Color(0xFF2A2F45), Color(0xFF2A2F45))))
            .clickable(enabled = enabled, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(icon, null, tint = if (enabled) Color.White else Palette.Muted)
            Spacer(Modifier.width(8.dp))
            Text(text, style = MaterialTheme.typography.labelLarge, color = if (enabled) Color.White else Palette.Muted)
        }
    }
}

@Composable
private fun FilesPanel(snap: Snapshot, root: File) {
    var expanded by remember { mutableStateOf(false) }
    val total = snap.requirements.size
    val have = total - snap.missing.size
    Panel(Modifier.animateContentSize()) {
        Row(Modifier.fillMaxWidth().clickable { expanded = !expanded }, verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("Файлы образа", style = MaterialTheme.typography.titleMedium, color = Palette.Text)
                Text(if (snap.ready) "Всё на месте" else "Есть $have из $total", style = MaterialTheme.typography.bodySmall,
                    color = if (snap.ready) Palette.Green else Palette.Amber)
            }
            Icon(if (expanded) Icons.Filled.KeyboardArrowUp else Icons.Filled.KeyboardArrowDown, null, tint = Palette.Muted)
        }
        if (total > 0) {
            Spacer(Modifier.height(12.dp))
            LinearProgressIndicator(
                progress = { have.toFloat() / total },
                modifier = Modifier.fillMaxWidth().height(6.dp).clip(CircleShape),
                color = if (snap.ready) Palette.Green else Palette.Blue,
                trackColor = Color(0xFF232945),
                drawStopIndicator = {},
            )
        }
        AnimatedVisibility(expanded) {
            Column(Modifier.padding(top = 12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                snap.requirements.forEach { r ->
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(if (r.present) Icons.Filled.CheckCircle else Icons.Filled.Close, null,
                            tint = if (r.present) Palette.Green else Palette.Red, modifier = Modifier.size(18.dp))
                        Spacer(Modifier.width(10.dp))
                        Column {
                            Text(r.label, style = MaterialTheme.typography.bodyMedium, color = Palette.Text)
                            Text(r.file.relativeTo(root).path, style = Mono, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        }
                    }
                }
                snap.rootImage?.let {
                    Text("Диск: ${it.file.name} (${it.format}), ${formatBytes(it.file.length())}",
                        style = MaterialTheme.typography.bodySmall, color = Palette.Muted)
                }
                Spacer(Modifier.height(4.dp))
                Text("Папка приложения — можно копировать туда и с компьютера по USB:", style = MaterialTheme.typography.bodySmall, color = Palette.Muted)
                SelectionContainer { Text(root.path, style = Mono.copy(color = Palette.Text)) }
            }
        }
    }
}

@Composable
private fun ImportPanel(
    progress: ImportProgress,
    onFolder: () -> Unit, onZip: () -> Unit, onFile: () -> Unit,
    onCancel: () -> Unit, onDismiss: () -> Unit,
) {
    Panel(Modifier.animateContentSize()) {
        Text("Импорт", style = MaterialTheme.typography.titleMedium, color = Palette.Text)
        Text("Папка или zip с InfernoData и AppleSEPROM-Cebu-B1. Пустые места большого диска не занимают памяти.",
            style = MaterialTheme.typography.bodySmall, color = Palette.Muted, modifier = Modifier.padding(top = 2.dp, bottom = 14.dp))
        if (progress.running) {
            Text(progress.title, style = MaterialTheme.typography.bodyMedium, color = Palette.Text)
            Text(progress.file, style = Mono, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Spacer(Modifier.height(10.dp))
            LinearProgressIndicator(
                progress = { progress.fraction },
                modifier = Modifier.fillMaxWidth().height(8.dp).clip(CircleShape),
                color = Palette.Violet, trackColor = Color(0xFF232945), drawStopIndicator = {},
            )
            Row(Modifier.fillMaxWidth().padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (progress.total > 0) "${formatBytes(progress.done)} из ${formatBytes(progress.total)} · ${(progress.fraction * 100).toInt()}%"
                    else formatBytes(progress.done),
                    style = MaterialTheme.typography.bodySmall, color = Palette.Muted, modifier = Modifier.weight(1f),
                )
                TextButton(onClick = onCancel) { Text("Отменить") }
            }
        } else {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                ImportButton("Папка", Glyphs.Folder, onFolder)
                ImportButton("ZIP", Glyphs.Archive, onZip)
                ImportButton("Файл", Glyphs.File, onFile)
            }
        }
        progress.message?.let { msg ->
            Row(
                Modifier.fillMaxWidth().padding(top = 12.dp).clip(RoundedCornerShape(14.dp))
                    .background((if (progress.failed) Palette.Red else Palette.Green).copy(alpha = 0.12f)).padding(12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(if (progress.failed) Icons.Filled.Warning else Icons.Filled.CheckCircle, null,
                    tint = if (progress.failed) Palette.Red else Palette.Green, modifier = Modifier.size(18.dp))
                Spacer(Modifier.width(10.dp))
                Text(msg, style = MaterialTheme.typography.bodySmall, color = Palette.Text, modifier = Modifier.weight(1f))
                IconButton(onClick = onDismiss, modifier = Modifier.size(28.dp)) { Icon(Icons.Filled.Close, null, tint = Palette.Muted) }
            }
        }
    }
}

@Composable
private fun RowScope.ImportButton(text: String, icon: ImageVector, onClick: () -> Unit) {
    FilledTonalButton(
        onClick = onClick, modifier = Modifier.weight(1f).height(64.dp), shape = RoundedCornerShape(16.dp),
        colors = ButtonDefaults.filledTonalButtonColors(containerColor = Color(0xFF1E2440), contentColor = Palette.Text),
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Icon(icon, null, Modifier.size(20.dp), tint = Palette.Blue)
            Text(text, fontSize = 13.sp)
        }
    }
}

@Composable
private fun SettingsPanel(s: VmSettings, deviceGb: Int, update: (VmSettings) -> Unit) {
    var advanced by remember { mutableStateOf(false) }
    Panel(Modifier.animateContentSize()) {
        Text("Настройки", style = MaterialTheme.typography.titleMedium, color = Palette.Text)
        Spacer(Modifier.height(14.dp))

        Label("Ядра", "${s.cores - 1} для iOS + 1 для SEP")
        Slider(value = s.cores.toFloat(), onValueChange = { update(s.copy(cores = it.toInt())) },
            valueRange = 2f..8f, steps = 5)

        Label("Память", if (s.memoryGb > deviceGb - 2) "у телефона всего ~$deviceGb ГБ — может не хватить" else "оперативка гостя")
        val sizes = listOf(2, 3, 4, 6)
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
            sizes.forEachIndexed { i, gb ->
                SegmentedButton(selected = s.memoryGb == gb, onClick = { update(s.copy(memoryGb = gb)) },
                    shape = SegmentedButtonDefaults.itemShape(i, sizes.size)) { Text("$gb ГБ") }
            }
        }

        Label("Экран", if (s.halfResolution) "414×896 — в 4 раза меньше пикселей, быстрее" else "828×1792 — как у настоящего iPhone 11")
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
            SegmentedButton(selected = s.halfResolution, onClick = { update(s.copy(halfResolution = true)) },
                shape = SegmentedButtonDefaults.itemShape(0, 2)) { Text("Быстрее") }
            SegmentedButton(selected = !s.halfResolution, onClick = { update(s.copy(halfResolution = false)) },
                shape = SegmentedButtonDefaults.itemShape(1, 2)) { Text("Чётче") }
        }

        Toggle("Интернет в iOS", "через USB-сеть машины (NAT)", s.network) { update(s.copy(network = it)) }
        Toggle("Кнопки громкости", "громкость телефона жмёт кнопки iPhone", s.volumeKeys) { update(s.copy(volumeKeys = it)) }
        Toggle("Вибрация", "Taptic Engine гостя → вибромотор", s.haptics) { update(s.copy(haptics = it)) }
        Toggle("Счётчик FPS", "поверх экрана", s.showFps) { update(s.copy(showFps = it)) }

        TextButton(onClick = { advanced = !advanced }, contentPadding = ButtonDefaults.TextButtonContentPadding) {
            Text(if (advanced) "Скрыть дополнительные" else "Дополнительно")
        }
        AnimatedVisibility(advanced) {
            Column {
                Label("Кеш трансляции", "${s.tbSizeMb} МБ")
                Slider(value = s.tbSizeMb.toFloat(), onValueChange = { update(s.copy(tbSizeMb = (it / 32).toInt() * 32)) },
                    valueRange = 64f..512f, steps = 13)
                Toggle("split-wx", "если движок падает на выделении памяти под JIT", s.splitWx) { update(s.copy(splitWx = it)) }
                OutlinedTextField(
                    value = s.bootArgs, onValueChange = { update(s.copy(bootArgs = it)) },
                    label = { Text("boot-args") }, placeholder = { Text(VmConfig.DEFAULT_BOOT_ARGS, style = Mono) },
                    textStyle = Mono.copy(color = Palette.Text), modifier = Modifier.fillMaxWidth(), minLines = 2,
                    trailingIcon = { if (s.bootArgs.isNotEmpty()) IconButton(onClick = { update(s.copy(bootArgs = "")) }) { Icon(Icons.Filled.Refresh, "Сбросить") } },
                )
            }
        }
    }
}

@Composable
private fun Label(title: String, hint: String) {
    Row(Modifier.fillMaxWidth().padding(top = 6.dp), verticalAlignment = Alignment.Bottom) {
        Text(title, style = MaterialTheme.typography.bodyMedium, color = Palette.Text, fontWeight = FontWeight.Medium)
        Spacer(Modifier.width(8.dp))
        Text(hint, style = MaterialTheme.typography.bodySmall, color = Palette.Muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

@Composable
private fun Toggle(title: String, hint: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth().clickable { onChange(!checked) }.padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyMedium, color = Palette.Text)
            Text(hint, style = MaterialTheme.typography.bodySmall, color = Palette.Muted)
        }
        Switch(checked = checked, onCheckedChange = onChange)
    }
}

@Composable
private fun LogsPanel(snap: Snapshot) {
    var console by remember { mutableStateOf(false) }
    val text = if (console) snap.consoleLog else snap.engineLog
    Panel {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(Glyphs.Terminal, null, tint = Palette.Blue, modifier = Modifier.size(20.dp))
            Spacer(Modifier.width(8.dp))
            Text("Журнал", style = MaterialTheme.typography.titleMedium, color = Palette.Text, modifier = Modifier.weight(1f))
            snap.lastExit?.let { Text("выход: $it", style = MaterialTheme.typography.bodySmall, color = Palette.Muted) }
        }
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(vertical = 10.dp)) {
            SegmentedButton(selected = !console, onClick = { console = false }, shape = SegmentedButtonDefaults.itemShape(0, 2)) { Text("Движок") }
            SegmentedButton(selected = console, onClick = { console = true }, shape = SegmentedButtonDefaults.itemShape(1, 2)) { Text("Консоль iOS") }
        }
        Box(
            Modifier.fillMaxWidth().heightIn(min = 80.dp, max = 280.dp).clip(RoundedCornerShape(14.dp))
                .background(Color(0xFF0A0C14)).border(1.dp, Palette.CardEdge, RoundedCornerShape(14.dp)).padding(12.dp),
        ) {
            SelectionContainer(Modifier.verticalScroll(rememberScrollState(Int.MAX_VALUE))) {
                Text(text.ifEmpty { "пока пусто" }, style = Mono)
            }
        }
    }
}
