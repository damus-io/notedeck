package com.damus.notedeck;

import android.Manifest;
import android.content.ClipData;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import android.util.Log;
import android.view.MotionEvent;
import android.view.View;
import android.view.ViewGroup;

import androidx.core.app.ActivityCompat;
import androidx.core.content.ContextCompat;
import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsCompat;
import androidx.core.view.WindowInsetsControllerCompat;

import com.damus.notedeck.service.NotificationsService;
import com.google.androidgamesdk.GameActivity;

import java.io.ByteArrayOutputStream;
import java.io.FileDescriptor;
import java.io.IOException;
import java.io.InputStream;

public class MainActivity extends GameActivity {
    private static final String TAG = "MainActivity";
    private static final String PREFS_NAME = "notedeck_prefs";
    private static final String PREF_NOTIFICATIONS_ENABLED = "notifications_enabled";
    private static final String PREF_ACTIVE_PUBKEY = "active_pubkey";
    private static final String PREF_RELAY_URLS = "relay_urls";
    private static final int REQUEST_CODE_PICK_FILE = 420;
    private static final int REQUEST_CODE_NOTIFICATION_PERMISSION = 421;

    // Native callbacks for file picker
    private native void nativeOnFilePickedFailed(String uri, String e);
    private native void nativeOnFilePickedWithContent(Object[] uri_info, byte[] content);

    // Native callback for notification permission result
    private native void nativeOnNotificationPermissionResult(boolean granted);

    // =========================================================================
    // Notification Control Methods (called from Rust via JNI)
    // =========================================================================

    /**
     * Enable push notifications for the given pubkey and relay URLs.
     * Writes settings to SharedPreferences and starts the notification service.
     * @param pubkeyHex The user's public key in hex format
     * @param relaysJson JSON array of relay URLs (e.g., ["wss://relay.damus.io", "wss://nos.lol"])
     */
    public void enableNotifications(String pubkeyHex, String relaysJson) {
        Log.d(TAG, "Enabling notifications for pubkey: " + pubkeyHex.substring(0, 8) + "...");

        SharedPreferences prefs = getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE);
        prefs.edit()
            .putBoolean(PREF_NOTIFICATIONS_ENABLED, true)
            .putString(PREF_ACTIVE_PUBKEY, pubkeyHex)
            .putString(PREF_RELAY_URLS, relaysJson)
            .apply();

        NotificationsService.start(this);
    }

    /**
     * Disable push notifications.
     * Stops the notification service and updates SharedPreferences.
     */
    public void disableNotifications() {
        Log.d(TAG, "Disabling notifications");

        SharedPreferences prefs = getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE);
        prefs.edit()
            .putBoolean(PREF_NOTIFICATIONS_ENABLED, false)
            .apply();

        NotificationsService.stop(this);
    }

    /**
     * Check if notification permission is granted.
     * On Android 13+, requires POST_NOTIFICATIONS permission.
     */
    public boolean isNotificationPermissionGranted() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            return ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS)
                == PackageManager.PERMISSION_GRANTED;
        }
        return true; // Permission not required on older Android versions
    }

    /**
     * Request notification permission from the user.
     * On Android 13+, shows the system permission dialog.
     * Result is delivered via nativeOnNotificationPermissionResult callback.
     */
    public void requestNotificationPermission() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            if (!isNotificationPermissionGranted()) {
                Log.d(TAG, "Requesting POST_NOTIFICATIONS permission");
                ActivityCompat.requestPermissions(
                    this,
                    new String[]{Manifest.permission.POST_NOTIFICATIONS},
                    REQUEST_CODE_NOTIFICATION_PERMISSION
                );
                return;
            }
        }
        // Already granted or not needed
        nativeOnNotificationPermissionResult(true);
    }

    /**
     * Check if notifications are currently enabled in preferences.
     */
    public boolean areNotificationsEnabled() {
        SharedPreferences prefs = getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE);
        return prefs.getBoolean(PREF_NOTIFICATIONS_ENABLED, false);
    }

    /**
     * Check if the notification service is currently running.
     */
    public boolean isNotificationServiceRunning() {
        return NotificationsService.isServiceRunning();
    }

  public void openFilePicker() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.setType("*/*");
        intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        startActivityForResult(intent, REQUEST_CODE_PICK_FILE);
  }

  private void setupInsets() {

      // NOTE(jb55): This is needed for keyboard visibility. Without this the
      // window still gets the right insets, but they’re consumed before they
      // reach the NDK side.
      //WindowCompat.setDecorFitsSystemWindows(getWindow(), false);

      // NOTE(jb55): This is needed for keyboard visibility. If the bars are
      // permanently gone, Android routes the keyboard over the GL surface and
      // doesn’t change insets.
      //WindowInsetsControllerCompat ic = WindowCompat.getInsetsController(getWindow(), getWindow().getDecorView());
      //ic.setSystemBarsBehavior(WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);

      View content = getContent();
      ViewCompat.setOnApplyWindowInsetsListener(content, (v, windowInsets) -> {
        Insets insets = windowInsets.getInsets(WindowInsetsCompat.Type.systemBars());

        ViewGroup.MarginLayoutParams mlp = (ViewGroup.MarginLayoutParams) v.getLayoutParams();
        mlp.topMargin = insets.top;
        mlp.leftMargin = insets.left;
        mlp.bottomMargin = insets.bottom;
        mlp.rightMargin = insets.right;
        v.setLayoutParams(mlp);

        return windowInsets;
      });

      WindowCompat.setDecorFitsSystemWindows(getWindow(), false);
  }

  private void processSelectedFile(Uri uri) {
        try {
            nativeOnFilePickedWithContent(this.getUriInfo(uri), readUriContent(uri));
        } catch (Exception e) {
            Log.e("MainActivity", "Error processing file: " + uri.toString(), e);

            nativeOnFilePickedFailed(uri.toString(), e.toString());
        }
  }

    private Object[] getUriInfo(Uri uri) throws Exception {
        if (!uri.getScheme().equals("content")) {
            throw new Exception("uri should start with content://");
        }

        Cursor cursor = getContentResolver().query(uri, null, null, null, null);

        while (cursor.moveToNext()) {
            Object[] info = new Object[3];

            int col_idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME);
            info[0] = cursor.getString(col_idx);

            col_idx = cursor.getColumnIndex(OpenableColumns.SIZE);
            info[1] = cursor.getLong(col_idx);

            col_idx = cursor.getColumnIndex("mime_type");
            info[2] = cursor.getString(col_idx);

            return info;
        }

        return null;
    }

    private byte[] readUriContent(Uri uri) {
        InputStream inputStream = null;
        ByteArrayOutputStream buffer = null;

        try {
            inputStream = getContentResolver().openInputStream(uri);
            if (inputStream == null) {
                Log.e("MainActivity", "Could not open input stream for URI: " + uri);
                return null;
            }

            buffer = new ByteArrayOutputStream();
            byte[] data = new byte[8192]; // 8KB buffer
            int bytesRead;

            while ((bytesRead = inputStream.read(data)) != -1) {
                buffer.write(data, 0, bytesRead);
            }

            byte[] result = buffer.toByteArray();
            Log.d("MainActivity", "Successfully read " + result.length + " bytes");
            return result;

        } catch (IOException e) {
            Log.e("MainActivity", "IOException while reading URI: " + uri, e);
            return null;
        } catch (SecurityException e) {
            Log.e("MainActivity", "SecurityException while reading URI: " + uri, e);
            return null;
        } finally {
            // Close streams
            if (inputStream != null) {
                try {
                    inputStream.close();
                } catch (IOException e) {
                    Log.e("MainActivity", "Error closing input stream", e);
                }
            }
            if (buffer != null) {
                try {
                    buffer.close();
                } catch (IOException e) {
                    Log.e("MainActivity", "Error closing buffer", e);
                }
            }
        }
    }

    // Native callback for deep links from notifications
    private native void nativeOnDeepLink(String eventId, int eventKind, String authorPubkey);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // Shrink view so it does not get covered by insets.

        setupInsets();
        //setupFullscreen()

        super.onCreate(savedInstanceState);

        // Handle deep link if launched from notification
        handleDeepLink(getIntent());

        // Start notification service if enabled in preferences (e.g., after force-stop or app restart)
        restartNotificationServiceIfEnabled();
    }

    /**
     * Restart notification service if it was enabled but not running.
     * This handles cases where the service was stopped (force-stop, crash) but preferences say enabled.
     */
    private void restartNotificationServiceIfEnabled() {
        SharedPreferences prefs = getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE);
        boolean enabled = prefs.getBoolean(PREF_NOTIFICATIONS_ENABLED, false);
        String pubkey = prefs.getString(PREF_ACTIVE_PUBKEY, null);

        if (enabled && pubkey != null && !pubkey.isEmpty()) {
            if (!NotificationsService.isServiceRunning()) {
                Log.i(TAG, "Restarting notification service (was enabled but not running)");
                NotificationsService.start(this);
            }
        }
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        // Handle deep link when app is already running
        handleDeepLink(intent);
    }

    /**
     * Process intent extras from notification deep links.
     * Passes event info to Rust for navigation.
     */
    private void handleDeepLink(Intent intent) {
        if (intent == null) return;

        String eventId = intent.getStringExtra("event_id");
        if (eventId == null) return;

        int eventKind = intent.getIntExtra("event_kind", -1);
        String authorPubkey = intent.getStringExtra("author_pubkey");

        Log.d(TAG, "Deep link: event_id=" + eventId.substring(0, 8) + ", kind=" + eventKind);

        try {
            nativeOnDeepLink(eventId, eventKind, authorPubkey != null ? authorPubkey : "");
        } catch (UnsatisfiedLinkError e) {
            Log.e(TAG, "Native deep link handler not available", e);
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);

        if (requestCode == REQUEST_CODE_NOTIFICATION_PERMISSION) {
            boolean granted = grantResults.length > 0
                && grantResults[0] == PackageManager.PERMISSION_GRANTED;
            Log.d(TAG, "Notification permission " + (granted ? "granted" : "denied"));
            nativeOnNotificationPermissionResult(granted);
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);

        if (requestCode == REQUEST_CODE_PICK_FILE && resultCode == RESULT_OK) {
            if (data == null) return;

            if (data.getClipData() != null) {
                // Multiple files selected
                ClipData clipData = data.getClipData();
                for (int i = 0; i < clipData.getItemCount(); i++) {
                    Uri uri = clipData.getItemAt(i).getUri();
                    processSelectedFile(uri);
                }
            } else if (data.getData() != null) {
                // Single file selected
                Uri uri = data.getData();
                processSelectedFile(uri);
            }
        }
    }

    private void setupFullscreen() {
        WindowCompat.setDecorFitsSystemWindows(getWindow(), false);

        WindowInsetsControllerCompat controller =
                WindowCompat.getInsetsController(getWindow(), getWindow().getDecorView());
        if (controller != null) {
            controller.setSystemBarsBehavior(
                    WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            );
            controller.hide(WindowInsetsCompat.Type.systemBars());
        }

        //focus(getContent())
    }

    // not sure if this does anything
    private void focus(View content) {
        content.setFocusable(true);
        content.setFocusableInTouchMode(true);
        content.requestFocus();
    }

    private View getContent() {
        return getWindow().getDecorView().findViewById(android.R.id.content);
    }

    @Override
    public void onResume() {
        super.onResume();
    }

    @Override
    public void onPause() {
        super.onPause();
    }

    @Override
    public void onDestroy() {
        super.onDestroy();
    }

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        // Offset the location so it fits the view with margins caused by insets.

        int[] location = new int[2];
        findViewById(android.R.id.content).getLocationOnScreen(location);
        event.offsetLocation(-location[0], -location[1]);

        return super.onTouchEvent(event);
    }
}
