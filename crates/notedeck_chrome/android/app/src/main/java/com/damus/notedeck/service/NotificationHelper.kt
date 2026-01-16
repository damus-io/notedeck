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
import java.net.URL
import java.util.concurrent.ConcurrentHashMap

/**
 * Helper class for creating rich Android notifications from Nostr events.
 */
object NotificationHelper {
    private const val TAG = "NotificationHelper"

    // Cache for profile images - thread-safe for concurrent access
    private val profileImageCache = ConcurrentHashMap<String, Bitmap>()

    /**
     * Create and show a notification for a Nostr event.
     * Receives structured data directly from Rust via JNI - no JSON parsing needed.
     *
     * @param eventId The 64-char hex event ID
     * @param eventKind The Nostr event kind
     * @param authorPubkey The 64-char hex pubkey of the event author
     * @param content The event content (already extracted in Rust)
     * @param authorName Optional display name of the author
     * @param authorPictureUrl Optional profile picture URL of the author
     * @param zapAmountSats Zap amount in satoshis (null if not a zap or amount unknown)
     */
    suspend fun showNotification(
        context: Context,
        eventId: String,
        eventKind: Int,
        authorPubkey: String,
        content: String,
        authorName: String?,
        authorPictureUrl: String? = null,
        zapAmountSats: Long? = null
    ) {
        val notificationManager = context.getSystemService(NotificationManager::class.java)

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
                val amountText = if (zapAmountSats != null && zapAmountSats > 0) {
                    formatSatsAmount(zapAmountSats)
                } else {
                    "some sats"
                }
                NotificationContent(
                    NotificationsService.CHANNEL_ZAPS,
                    displayName,
                    "Zapped you $amountText!",
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
            putExtra("event_id", eventId)
            putExtra("event_kind", eventKind)
            putExtra("author_pubkey", authorPubkey)
        }

        val pendingIntent = PendingIntent.getActivity(
            context,
            eventId.hashCode(),
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        // Load profile image (use real URL if available, fall back to robohash)
        val bitmap = loadProfileImage(authorPubkey, authorPictureUrl)

        // Create person for messaging style
        val person = Person.Builder()
            .setKey(authorPubkey)
            .setName(title)
            .apply {
                if (bitmap != null) {
                    setIcon(IconCompat.createWithBitmap(bitmap))
                }
            }
            .build()

        // Build notification
        val builder = NotificationCompat.Builder(context, channel)
            .setSmallIcon(R.mipmap.damusfg)
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
            .setSmallIcon(R.mipmap.damusfg)
            .setContentTitle(summaryTitle)
            .setGroup(groupKey)
            .setGroupSummary(true)
            .setAutoCancel(true)
            .build()

        notificationManager.notify(groupKey.hashCode(), summary)
    }

    /**
     * Load a profile image from the provided URL or robohash (fallback).
     * Thread-safe via ConcurrentHashMap.
     *
     * @param pubkey The author's pubkey (used as cache key and robohash fallback)
     * @param pictureUrl Optional real profile picture URL from the author's profile
     */
    private suspend fun loadProfileImage(pubkey: String, pictureUrl: String?): Bitmap? {
        // Check cache first (thread-safe read)
        profileImageCache[pubkey]?.let { return it }

        return withContext(Dispatchers.IO) {
            try {
                // Double-check after acquiring IO context (another thread may have loaded it)
                profileImageCache[pubkey]?.let { return@withContext it }

                // Use real picture URL if available, fall back to robohash
                val imageUrl = if (!pictureUrl.isNullOrEmpty()) {
                    Log.i(TAG, "Loading real profile image for ${pubkey.take(8)}: ${pictureUrl.take(50)}...")
                    URL(pictureUrl)
                } else {
                    Log.i(TAG, "No profile image URL, using robohash for ${pubkey.take(8)}")
                    URL("https://robohash.org/${pubkey}.png?size=128x128&set=set4")
                }

                val connection = imageUrl.openConnection()
                connection.connectTimeout = 5000
                connection.readTimeout = 5000
                val bitmap = BitmapFactory.decodeStream(connection.getInputStream())

                // Cache the result (thread-safe write via putIfAbsent)
                if (bitmap != null) {
                    profileImageCache.putIfAbsent(pubkey, bitmap)
                }
                bitmap
            } catch (e: Exception) {
                Log.w(TAG, "Failed to load profile image for $pubkey", e)
                // Try robohash as final fallback if real URL failed
                if (!pictureUrl.isNullOrEmpty()) {
                    try {
                        val fallbackUrl = URL("https://robohash.org/${pubkey}.png?size=128x128&set=set4")
                        val connection = fallbackUrl.openConnection()
                        connection.connectTimeout = 5000
                        connection.readTimeout = 5000
                        val bitmap = BitmapFactory.decodeStream(connection.getInputStream())
                        if (bitmap != null) {
                            profileImageCache.putIfAbsent(pubkey, bitmap)
                        }
                        return@withContext bitmap
                    } catch (e2: Exception) {
                        Log.w(TAG, "Robohash fallback also failed for $pubkey", e2)
                    }
                }
                null
            }
        }
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

    /**
     * Format satoshi amount for display.
     * Shows "1,000 sats" for smaller amounts or "1.5M sats" for larger.
     */
    private fun formatSatsAmount(sats: Long): String {
        return when {
            sats >= 1_000_000_000 -> String.format("%.1fB sats", sats / 1_000_000_000.0)
            sats >= 1_000_000 -> String.format("%.1fM sats", sats / 1_000_000.0)
            sats >= 10_000 -> String.format("%,d sats", sats)
            else -> "$sats sats"
        }
    }

    private data class NotificationContent(
        val channel: String,
        val title: String,
        val text: String,
        val groupKey: String
    )
}
