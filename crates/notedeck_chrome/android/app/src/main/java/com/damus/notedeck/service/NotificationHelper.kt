package com.damus.notedeck.service

import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.graphics.drawable.IconCompat
import com.damus.notedeck.MainActivity
import com.damus.notedeck.R
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.net.URL
import java.security.MessageDigest

/**
 * Helper class for creating rich Android notifications from Nostr events.
 */
object NotificationHelper {
    private const val TAG = "NotificationHelper"

    // Cache for profile images
    private val profileImageCache = mutableMapOf<String, Bitmap?>()

    /**
     * Create and show a notification for a Nostr event.
     */
    suspend fun showNotification(
        context: Context,
        eventJson: String,
        eventKind: Int,
        authorPubkey: String,
        authorName: String?
    ) {
        val notificationManager = context.getSystemService(NotificationManager::class.java)

        // Parse event details
        val event = try {
            JSONObject(eventJson.substringAfter("{").let { "{$it" })
        } catch (e: Exception) {
            Log.w(TAG, "Failed to parse event JSON", e)
            null
        }

        val eventId = event?.optString("id") ?: authorPubkey.hashCode().toString()
        val content = event?.optString("content") ?: ""

        // Determine notification channel and content based on event kind
        val (channel, title, text, groupKey) = when (eventKind) {
            1 -> {
                // Text note mention
                val displayName = authorName ?: formatPubkey(authorPubkey)
                NotificationContent(
                    NotificationsService.CHANNEL_NOTIFICATIONS,
                    displayName,
                    truncateContent(content, 100).ifEmpty { "Mentioned you in a note" },
                    "mentions"
                )
            }
            4 -> {
                // Legacy DM
                val displayName = authorName ?: formatPubkey(authorPubkey)
                NotificationContent(
                    NotificationsService.CHANNEL_DMS,
                    displayName,
                    "Sent you a direct message",
                    "dms"
                )
            }
            6 -> {
                // Repost
                val displayName = authorName ?: formatPubkey(authorPubkey)
                NotificationContent(
                    NotificationsService.CHANNEL_NOTIFICATIONS,
                    displayName,
                    "Reposted your note",
                    "reposts"
                )
            }
            7 -> {
                // Reaction
                val displayName = authorName ?: formatPubkey(authorPubkey)
                val reaction = content.ifEmpty { "+" }
                NotificationContent(
                    NotificationsService.CHANNEL_NOTIFICATIONS,
                    displayName,
                    "Reacted $reaction to your note",
                    "reactions"
                )
            }
            1059 -> {
                // Gift-wrapped DM (NIP-17)
                val displayName = authorName ?: formatPubkey(authorPubkey)
                NotificationContent(
                    NotificationsService.CHANNEL_DMS,
                    displayName,
                    "Sent you a private message",
                    "dms"
                )
            }
            9735 -> {
                // Zap receipt
                val displayName = authorName ?: formatPubkey(authorPubkey)
                val amount = parseZapAmount(event)
                val amountText = if (amount != null) "${amount} sats" else "some sats"
                NotificationContent(
                    NotificationsService.CHANNEL_ZAPS,
                    displayName,
                    "Zapped you $amountText! ⚡",
                    "zaps"
                )
            }
            else -> {
                NotificationContent(
                    NotificationsService.CHANNEL_NOTIFICATIONS,
                    "Nostr",
                    "New notification",
                    "other"
                )
            }
        }

        // Create intent to open app
        val intent = Intent(context, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            putExtra("nostr_event", eventJson)
            putExtra("event_id", eventId)
        }

        val pendingIntent = PendingIntent.getActivity(
            context,
            eventId.hashCode(),
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        // Create person for messaging style
        val person = Person.Builder()
            .setKey(authorPubkey)
            .setName(title)
            .apply {
                // Try to load profile image
                val bitmap = loadProfileImage(authorPubkey)
                if (bitmap != null) {
                    setIcon(IconCompat.createWithBitmap(bitmap))
                }
            }
            .build()

        // Build notification
        val builder = NotificationCompat.Builder(context, channel)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle(title)
            .setContentText(text)
            .setContentIntent(pendingIntent)
            .setAutoCancel(true)
            .setGroup(groupKey)
            .setPriority(
                when (channel) {
                    NotificationsService.CHANNEL_DMS -> NotificationCompat.PRIORITY_HIGH
                    NotificationsService.CHANNEL_ZAPS -> NotificationCompat.PRIORITY_DEFAULT
                    else -> NotificationCompat.PRIORITY_DEFAULT
                }
            )

        // Add profile image as large icon if available
        val bitmap = loadProfileImage(authorPubkey)
        if (bitmap != null) {
            builder.setLargeIcon(bitmap)
        }

        // Add actions
        builder.addAction(
            0,
            "Open",
            pendingIntent
        )

        // Add mute action
        val muteIntent = Intent(context, NotificationActionReceiver::class.java).apply {
            action = NotificationActionReceiver.ACTION_MUTE_USER
            putExtra(NotificationActionReceiver.EXTRA_PUBKEY, authorPubkey)
        }
        val mutePendingIntent = PendingIntent.getBroadcast(
            context,
            (authorPubkey + "mute").hashCode(),
            muteIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        builder.addAction(0, "Mute", mutePendingIntent)

        // Show the notification
        val notificationId = NotificationsService.NOTIFICATION_ID_BASE + (eventId.hashCode() and 0xFFFF)
        notificationManager.notify(notificationId, builder.build())

        // Also show a summary notification for grouping
        showGroupSummary(context, groupKey, channel)
    }

    private fun showGroupSummary(context: Context, groupKey: String, channel: String) {
        val notificationManager = context.getSystemService(NotificationManager::class.java)

        val summaryTitle = when (groupKey) {
            "mentions" -> "New mentions"
            "dms" -> "New messages"
            "reactions" -> "New reactions"
            "reposts" -> "New reposts"
            "zaps" -> "New zaps"
            else -> "New notifications"
        }

        val summary = NotificationCompat.Builder(context, channel)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle(summaryTitle)
            .setGroup(groupKey)
            .setGroupSummary(true)
            .setAutoCancel(true)
            .build()

        notificationManager.notify(groupKey.hashCode(), summary)
    }

    /**
     * Load a profile image from robohash (fallback) or cached.
     */
    private suspend fun loadProfileImage(pubkey: String): Bitmap? {
        // Check cache first
        profileImageCache[pubkey]?.let { return it }

        return withContext(Dispatchers.IO) {
            try {
                // Use robohash as a simple avatar generator
                val url = URL("https://robohash.org/${pubkey}.png?size=128x128&set=set4")
                val connection = url.openConnection()
                connection.connectTimeout = 5000
                connection.readTimeout = 5000
                val bitmap = BitmapFactory.decodeStream(connection.getInputStream())

                // Cache the result
                if (bitmap != null) {
                    profileImageCache[pubkey] = bitmap
                }
                bitmap
            } catch (e: Exception) {
                Log.w(TAG, "Failed to load profile image for $pubkey", e)
                null
            }
        }
    }

    /**
     * Parse zap amount from a kind 9735 event.
     */
    private fun parseZapAmount(event: JSONObject?): Long? {
        if (event == null) return null

        try {
            val tags = event.optJSONArray("tags") ?: return null
            for (i in 0 until tags.length()) {
                val tag = tags.optJSONArray(i) ?: continue
                if (tag.length() >= 2 && tag.optString(0) == "bolt11") {
                    val bolt11 = tag.optString(1)
                    return parseBolt11Amount(bolt11)
                }
            }
        } catch (e: Exception) {
            Log.w(TAG, "Failed to parse zap amount", e)
        }
        return null
    }

    /**
     * Parse amount from a BOLT11 invoice (simplified).
     */
    private fun parseBolt11Amount(bolt11: String): Long? {
        // BOLT11 format: lnbc<amount><multiplier>...
        // This is a simplified parser
        val match = Regex("lnbc(\\d+)([munp]?)").find(bolt11.lowercase())
        if (match != null) {
            val amount = match.groupValues[1].toLongOrNull() ?: return null
            val multiplier = when (match.groupValues[2]) {
                "m" -> 100_000L  // milli-bitcoin = 100,000 sats
                "u" -> 100L      // micro-bitcoin = 100 sats
                "n" -> 0L        // nano-bitcoin = 0.1 sats (round down)
                "p" -> 0L        // pico-bitcoin
                else -> 100_000_000L  // assume whole bitcoin
            }
            return amount * multiplier / 1000  // Convert to sats
        }
        return null
    }

    /**
     * Format a pubkey for display (first 8 chars).
     */
    private fun formatPubkey(pubkey: String): String {
        return if (pubkey.length >= 8) {
            "${pubkey.substring(0, 8)}..."
        } else {
            pubkey
        }
    }

    /**
     * Truncate content to a maximum length.
     */
    private fun truncateContent(content: String, maxLength: Int): String {
        return if (content.length > maxLength) {
            "${content.substring(0, maxLength)}..."
        } else {
            content
        }
    }

    /**
     * Clear the profile image cache.
     */
    fun clearCache() {
        profileImageCache.clear()
    }

    private data class NotificationContent(
        val channel: String,
        val title: String,
        val text: String,
        val groupKey: String
    )
}
