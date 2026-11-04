package org.lighttouch.inferno

import android.annotation.SuppressLint
import android.content.Intent
import android.graphics.SurfaceTexture
import android.os.Bundle
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.Surface
import android.view.TextureView
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback
import androidx.activity.compose.setContent
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import org.lighttouch.inferno.ui.Glyphs
import org.lighttouch.inferno.ui.InfernoTheme
import org.lighttouch.inferno.ui.Mono
import org.lighttouch.inferno.ui.Palette

/**
 * The guest's screen. Lives in the :vm process beside [VmService]; leaving it
 * does not stop the machine — only the power-off button or the notification do.
 */
class VmActivity : ComponentActivity() {
    private lateinit var settings: VmSettings

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        settings = Prefs.load(this)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowCompat.getInsetsController(window, window.decorView).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
        // Back is the guest's Home button; leaving is the minimise button.
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                Engine.key(Engine.KEY_HOME, true)
                Engine.key(Engine.KEY_HOME, false)
            }
        })
        setContent { InfernoTheme { VmScreen(settings, onMinimize = ::minimize) } }
    }

    private fun minimize() {
        startActivity(Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT))
        finish()
    }

    private fun volumeKey(keyCode: Int) = when (keyCode) {
        KeyEvent.KEYCODE_VOLUME_UP -> Engine.KEY_VOLUME_UP
        KeyEvent.KEYCODE_VOLUME_DOWN -> Engine.KEY_VOLUME_DOWN
        else -> 0
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        val key = volumeKey(keyCode)
        if (key == 0 || !settings.volumeKeys) return super.onKeyDown(keyCode, event)
        if (event.repeatCount == 0) Engine.key(key, true)
        return true
    }

    override fun onKeyUp(keyCode: Int, event: KeyEvent): Boolean {
        val key = volumeKey(keyCode)
        if (key == 0 || !settings.volumeKeys) return super.onKeyUp(keyCode, event)
        Engine.key(key, false)
        return true
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun VmScreen(settings: VmSettings, onMinimize: () -> Unit) {
    val state by VmStatus.state.collectAsState()
    var firstFrame by remember { mutableStateOf(false) }
    var fps by remember { mutableStateOf("") }
    var net by remember { mutableStateOf(false) }
    var consoleOpen by remember { mutableStateOf(false) }
    var confirmOff by remember { mutableStateOf(false) }
    val context = androidx.compose.ui.platform.LocalContext.current

    LaunchedEffect(Unit) {
        while (true) {
            delay(1000)
            val stats = Engine.stats()
            if (stats[0] > 0) firstFrame = true
            fps = "${stats[0]} fps · гость ${stats[1]}"
            net = Engine.networkUp()
        }
    }

    BoxWithConstraints(Modifier.fillMaxSize().background(Color.Black)) {
        val landscape = maxWidth > maxHeight
        val aspect = settings.displayWidth.toFloat() / settings.displayHeight
        if (landscape) {
            Row(Modifier.fillMaxSize().padding(12.dp), verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.Center) {
                Box(Modifier.weight(1f).fillMaxSize(), contentAlignment = Alignment.Center) {
                    PhoneFrame(aspect, settings, Modifier.fillMaxSize())
                }
                Spacer(Modifier.width(12.dp))
                Controls(vertical = true, onConsole = { consoleOpen = true }, onPower = { confirmOff = true }, onMinimize = onMinimize)
            }
        } else {
            Column(Modifier.fillMaxSize().padding(horizontal = 12.dp, vertical = 16.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                    PhoneFrame(aspect, settings, Modifier.fillMaxSize())
                }
                Spacer(Modifier.height(14.dp))
                Controls(vertical = false, onConsole = { consoleOpen = true }, onPower = { confirmOff = true }, onMinimize = onMinimize)
            }
        }

        // What is going on, top left.
        Row(Modifier.padding(16.dp).align(Alignment.TopStart), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            if (settings.showFps && fps.isNotEmpty()) Badge(fps, Palette.Muted)
            if (net) Badge("сеть", Palette.Green)
        }

        AnimatedVisibility(!firstFrame && state !is VmStatus.State.Failed, enter = fadeIn(), exit = fadeOut(),
            modifier = Modifier.align(Alignment.Center)) {
            Column(horizontalAlignment = Alignment.CenterHorizontally,
                modifier = Modifier.clip(RoundedCornerShape(20.dp)).background(Color(0xCC0E1120)).padding(24.dp)) {
                CircularProgressIndicator(color = Palette.Blue, strokeWidth = 3.dp, modifier = Modifier.size(36.dp))
                Spacer(Modifier.height(14.dp))
                Text(if (state is VmStatus.State.Running) "Загружается iOS…" else "Запуск машины…", color = Palette.Text)
                Text("первая загрузка может занять несколько минут", style = MaterialTheme.typography.bodySmall, color = Palette.Muted)
            }
        }

        (state as? VmStatus.State.Failed)?.let { failed ->
            Column(Modifier.align(Alignment.Center).padding(24.dp).clip(RoundedCornerShape(20.dp)).background(Palette.Card).padding(24.dp),
                horizontalAlignment = Alignment.CenterHorizontally) {
                Icon(Icons.Filled.Warning, null, tint = Palette.Red, modifier = Modifier.size(36.dp))
                Spacer(Modifier.height(10.dp))
                Text("Не получилось запустить", style = MaterialTheme.typography.titleMedium, color = Palette.Text)
                Text(failed.reason, style = MaterialTheme.typography.bodySmall, color = Palette.Muted)
            }
        }
    }

    if (consoleOpen) {
        ModalBottomSheet(onDismissRequest = { consoleOpen = false }, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
            containerColor = Color(0xFF0A0C14)) {
            ConsoleSheet()
        }
    }

    if (confirmOff) {
        AlertDialog(
            onDismissRequest = { confirmOff = false },
            title = { Text("Выключить машину?") },
            text = { Text("Это как выдернуть кабель: iOS не успеет ничего сохранить. Для обычного выключения подержи кнопку питания и смахни слайдер в самой iOS.") },
            confirmButton = { TextButton(onClick = { confirmOff = false; VmService.stop(context) }) { Text("Выключить", color = Palette.Red) } },
            dismissButton = { TextButton(onClick = { confirmOff = false }) { Text("Отмена") } },
        )
    }
}

@Composable
private fun Badge(text: String, color: Color) {
    Text(text, fontSize = 11.sp, color = color, fontWeight = FontWeight.Medium,
        modifier = Modifier.clip(CircleShape).background(Color(0x99000000)).padding(horizontal = 10.dp, vertical = 4.dp))
}

/** The guest's screen in an iPhone-shaped bezel. */
@Composable
private fun PhoneFrame(aspect: Float, settings: VmSettings, modifier: Modifier) {
    BoxWithConstraints(modifier, contentAlignment = Alignment.Center) {
        // Fit the screen's aspect ratio into whatever room there is.
        val byHeight = maxHeight * aspect
        val width = if (byHeight <= maxWidth) byHeight else maxWidth
        val bezel = (width * 0.035f).coerceIn(6.dp, 14.dp)
        val screenRadius = width * 0.11f
        Box(
            Modifier.width(width).aspectRatio(aspect)
                .clip(RoundedCornerShape(screenRadius + bezel))
                .background(Color(0xFF16181F))
                .border(1.5.dp, Color(0xFF3A3F55), RoundedCornerShape(screenRadius + bezel))
                .padding(bezel),
        ) {
            GuestScreen(settings, Modifier.fillMaxSize().clip(RoundedCornerShape(screenRadius)))
        }
    }
}

/** A TextureView the engine draws into; touches go back in guest pixels. */
@SuppressLint("ClickableViewAccessibility")
@Composable
private fun GuestScreen(settings: VmSettings, modifier: Modifier) {
    AndroidView(modifier = modifier.background(Color.Black), factory = { ctx ->
        TextureView(ctx).apply {
            isOpaque = true
            surfaceTextureListener = object : TextureView.SurfaceTextureListener {
                var surface: Surface? = null
                override fun onSurfaceTextureAvailable(st: SurfaceTexture, w: Int, h: Int) {
                    surface = Surface(st).also { Engine.setSurface(it) }
                }
                override fun onSurfaceTextureSizeChanged(st: SurfaceTexture, w: Int, h: Int) {}
                override fun onSurfaceTextureDestroyed(st: SurfaceTexture): Boolean {
                    Engine.setSurface(null)
                    surface?.release()
                    surface = null
                    return true
                }
                override fun onSurfaceTextureUpdated(st: SurfaceTexture) {}
            }
            var pointer = -1
            setOnTouchListener { view, e ->
                val size = Engine.screenSize()
                val fbW = if (size[0] > 0) size[0] else settings.displayWidth
                val fbH = if (size[1] > 0) size[1] else settings.displayHeight
                fun send(index: Int, pressed: Boolean) {
                    val x = (e.getX(index) / view.width * fbW).toInt().coerceIn(0, fbW - 1)
                    val y = (e.getY(index) / view.height * fbH).toInt().coerceIn(0, fbH - 1)
                    Engine.touch(x, y, pressed)
                }
                // The panel takes one finger; the first one down is the one followed.
                when (e.actionMasked) {
                    MotionEvent.ACTION_DOWN -> { pointer = e.getPointerId(0); send(0, true) }
                    MotionEvent.ACTION_MOVE -> e.findPointerIndex(pointer).takeIf { it >= 0 }?.let { send(it, true) }
                    MotionEvent.ACTION_POINTER_UP -> if (e.getPointerId(e.actionIndex) == pointer) { send(e.actionIndex, false); pointer = -1 }
                    MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                        if (pointer >= 0) e.findPointerIndex(pointer).takeIf { it >= 0 }?.let { send(it, false) }
                        pointer = -1
                    }
                }
                true
            }
        }
    })
}

@Composable
private fun Controls(vertical: Boolean, onConsole: () -> Unit, onPower: () -> Unit, onMinimize: () -> Unit) {
    val items: @Composable () -> Unit = {
        HoldButton(Glyphs.VolumeDown, Engine.KEY_VOLUME_DOWN)
        HoldButton(Glyphs.VolumeUp, Engine.KEY_VOLUME_UP)
        HomeButton()
        HoldButton(Glyphs.Power, Engine.KEY_POWER)
        RoundButton(Glyphs.Terminal, 44.dp, onClick = onConsole)
        RoundButton(Glyphs.Minimize, 44.dp, onClick = onMinimize)
        RoundButton(Icons.Filled.Close, 44.dp, tint = Palette.Red, onClick = onPower)
    }
    val shape = RoundedCornerShape(28.dp)
    val bar = Modifier.clip(shape).background(Color(0xFF12141C)).border(1.dp, Palette.CardEdge, shape).padding(8.dp)
    if (vertical) Column(bar, verticalArrangement = Arrangement.spacedBy(8.dp), horizontalAlignment = Alignment.CenterHorizontally) { items() }
    else Row(bar, horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) { items() }
}

/** A device button: pressed for as long as the finger is on it (a long Power press is iOS's power-off slider). */
@Composable
private fun HoldButton(icon: ImageVector, key: Int) {
    var down by remember { mutableStateOf(false) }
    Box(
        Modifier.size(44.dp).clip(CircleShape).background(if (down) Color(0xFF2E3550) else Color(0xFF1C2033))
            .pointerInput(key) {
                detectTapGestures(onPress = {
                    down = true
                    Engine.key(key, true)
                    tryAwaitRelease()
                    Engine.key(key, false)
                    down = false
                })
            },
        contentAlignment = Alignment.Center,
    ) { Icon(icon, null, tint = Palette.Text, modifier = Modifier.size(20.dp)) }
}

/** Home: a ring, as on the phones that had one. Held, it is a held Home. */
@Composable
private fun HomeButton() {
    var down by remember { mutableStateOf(false) }
    Box(
        Modifier.size(52.dp).clip(CircleShape).background(if (down) Color(0xFF2E3550) else Color(0xFF1C2033))
            .pointerInput(Unit) {
                detectTapGestures(onPress = {
                    down = true
                    Engine.key(Engine.KEY_HOME, true)
                    tryAwaitRelease()
                    Engine.key(Engine.KEY_HOME, false)
                    down = false
                })
            },
        contentAlignment = Alignment.Center,
    ) {
        Box(Modifier.size(26.dp).border(2.dp, Palette.Text.copy(alpha = 0.8f), RoundedCornerShape(8.dp)))
    }
}

@Composable
private fun RoundButton(icon: ImageVector, size: Dp, tint: Color = Palette.Muted, onClick: () -> Unit) {
    Box(
        Modifier.size(size).clip(CircleShape).background(Color(0xFF1C2033)).pointerInput(Unit) { detectTapGestures(onTap = { onClick() }) },
        contentAlignment = Alignment.Center,
    ) { Icon(icon, null, tint = tint, modifier = Modifier.size(20.dp)) }
}

@Composable
private fun ConsoleSheet() {
    val context = androidx.compose.ui.platform.LocalContext.current
    val config = remember { Prefs.config(context) }
    var engine by remember { mutableStateOf(false) }
    var lines by remember { mutableStateOf(emptyList<String>()) }
    val list = rememberLazyListState()
    LaunchedEffect(engine) {
        while (true) {
            val text = withContext(Dispatchers.IO) { tail(if (engine) config.engineLog else config.guestConsoleLog, 400) }
            val next = text.lines()
            if (next != lines) {
                lines = next
                if (next.isNotEmpty()) list.scrollToItem(next.lastIndex)
            }
            delay(1000)
        }
    }
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp).padding(bottom = 24.dp)) {
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(bottom = 10.dp)) {
            SegmentedButton(selected = !engine, onClick = { engine = false }, shape = SegmentedButtonDefaults.itemShape(0, 2)) { Text("Консоль iOS") }
            SegmentedButton(selected = engine, onClick = { engine = true }, shape = SegmentedButtonDefaults.itemShape(1, 2)) { Text("Движок") }
        }
        LazyColumn(Modifier.fillMaxWidth().height(420.dp), state = list) {
            items(lines) { Text(it, style = Mono.copy(color = Color(0xFFB8C0DA))) }
        }
    }
}
