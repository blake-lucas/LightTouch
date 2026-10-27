package org.lighttouch.android

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import org.json.JSONObject
import java.security.MessageDigest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

internal data class NoticeGroup(val folder: String, val files: List<Pair<String, String>>, val source: String)

internal fun checkedNoticePath(path: String): Boolean = path.split('/').all {
    it.isNotEmpty() && it != "." && it != ".." && it.matches(Regex("[A-Za-z0-9._-]+"))
}

@Composable
internal fun LicenseDialog(onClose: () -> Unit) {
    val context = LocalContext.current
    var groups by remember { mutableStateOf<List<NoticeGroup>>(emptyList()) }
    var selected by remember { mutableStateOf<Pair<NoticeGroup, Pair<String, String>>?>(null) }
    var text by remember { mutableStateOf("") }
    var failure by remember { mutableStateOf("") }
    LaunchedEffect(Unit) {
        try {
            groups = withContext(Dispatchers.IO) {
                (context.assets.list("licenses") ?: emptyArray()).map { folder ->
                    require(checkedNoticePath(folder) && !folder.contains('/'))
                    val manifest = JSONObject(context.assets.open("licenses/$folder/manifest.json").bufferedReader().use { it.readText() })
                    require(manifest.getInt("schema") == 1)
                    val files = manifest.getJSONArray("files")
                    NoticeGroup(folder, (0 until files.length()).map { index ->
                        val entry = files.getJSONObject(index)
                        val path = entry.getString("path"); val hash = entry.getString("sha256")
                        require(checkedNoticePath(path))
                        require(hash.matches(Regex("[0-9a-f]{64}")))
                        path to hash
                    }, manifest.getString("source_reference"))
                }
            }
        } catch (error: Exception) { failure = "Licenses could not be loaded: ${error.message ?: "unavailable"}" }
    }
    LaunchedEffect(selected) {
        text = ""
        val choice = selected ?: return@LaunchedEffect
        try {
            text = withContext(Dispatchers.IO) {
                val bytes = context.assets.open("licenses/${choice.first.folder}/${choice.second.first}").use { it.readBytes() }
                val digest = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
                require(digest == choice.second.second) { "Notice integrity check failed" }
                bytes.toString(Charsets.UTF_8)
            }
        } catch (error: Exception) { failure = "License could not be read: ${error.message ?: "unavailable"}" }
    }
    AlertDialog(onDismissRequest = onClose, title = { Text(selected?.second?.first ?: "Open source licenses") },
        text = {
            LazyColumn(Modifier.testTag("license_content")) {
                if (failure.isNotEmpty()) item { Text(failure, color = MaterialTheme.colorScheme.error) }
                if (selected == null) {
                    for (group in groups) {
                        item { Text(when (group.folder) { "firmware", "firmwarekit" -> "Firmware preparation"; "engine-arm64-v8a" -> "Engine (ARM64)"; "engine-x86_64" -> "Engine (x86-64)"; "angle" -> "Graphics provider (ANGLE)"; "frontend" -> "Android interface"; "network" -> "Network (OpenSSL)"; else -> group.folder }, style = MaterialTheme.typography.titleSmall) }
                        items(group.files) { file -> TextButton(onClick = { failure = ""; selected = group to file }) { Text(file.first) } }
                        item { TextButton(onClick = { openSource(context, group.source) { failure = it } }) { Text("Source code") } }
                    }
                } else items(text.chunked(1200)) { part -> Text(part, style = MaterialTheme.typography.bodySmall) }
            }
        },
        confirmButton = { TextButton(onClick = onClose) { Text("Close") } },
        dismissButton = { if (selected != null) TextButton(onClick = { selected = null; failure = "" }) { Text("All licenses") } })
}
private fun openSource(context: Context, reference: String, fail: (String) -> Unit) {
    try {
        val uri = Uri.parse(reference)
        require(uri.scheme == "https" && !uri.host.isNullOrEmpty() && uri.userInfo == null)
        context.startActivity(Intent(Intent.ACTION_VIEW, uri))
    } catch (error: Exception) { fail("Source page could not be opened: ${error.message ?: "no browser available"}") }
}
