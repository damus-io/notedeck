package com.damus.notedeck.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

/**
 * Handles notification actions like "Mark as Read", "Mute", etc.
 */
class NotificationActionReceiver : BroadcastReceiver() {

    companion object {
        private const val TAG = "NotificationAction"

        const val ACTION_MARK_READ = "com.damus.notedeck.MARK_READ"
        const val ACTION_MUTE_USER = "com.damus.notedeck.MUTE_USER"
        const val ACTION_MUTE_THREAD = "com.damus.notedeck.MUTE_THREAD"

        const val EXTRA_EVENT_ID = "event_id"
        const val EXTRA_PUBKEY = "pubkey"
        const val EXTRA_THREAD_ID = "thread_id"
    }

    override fun onReceive(context: Context, intent: Intent) {
        Log.d(TAG, "Received action: ${intent.action}")

        when (intent.action) {
            ACTION_MARK_READ -> {
                val eventId = intent.getStringExtra(EXTRA_EVENT_ID)
                if (eventId != null) {
                    markAsRead(context, eventId)
                }
            }
            ACTION_MUTE_USER -> {
                val pubkey = intent.getStringExtra(EXTRA_PUBKEY)
                if (pubkey != null) {
                    muteUser(context, pubkey)
                }
            }
            ACTION_MUTE_THREAD -> {
                val threadId = intent.getStringExtra(EXTRA_THREAD_ID)
                if (threadId != null) {
                    muteThread(context, threadId)
                }
            }
        }
    }

    private fun markAsRead(context: Context, eventId: String) {
        Log.d(TAG, "Marking event as read: $eventId")
        // TODO: Implement via JNI call to Rust
    }

    private fun muteUser(context: Context, pubkey: String) {
        Log.d(TAG, "Muting user: $pubkey")
        // TODO: Implement via JNI call to Rust - publish to mute list
    }

    private fun muteThread(context: Context, threadId: String) {
        Log.d(TAG, "Muting thread: $threadId")
        // TODO: Implement via JNI call to Rust - publish to mute list
    }
}
