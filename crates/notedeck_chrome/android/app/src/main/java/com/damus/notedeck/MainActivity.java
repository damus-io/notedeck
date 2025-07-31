package com.damus.notedeck;

import android.content.ClipData;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import android.util.Log;
import android.view.MotionEvent;
import android.view.View;
import android.view.ViewGroup;

import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsCompat;
import androidx.core.view.WindowInsetsControllerCompat;

import com.google.androidgamesdk.GameActivity;

import java.io.ByteArrayOutputStream;
import java.io.FileDescriptor;
import java.io.IOException;
import java.io.InputStream;

public class MainActivity extends GameActivity {
    static final int REQUEST_CODE_PICK_FILE = 420;

    static {
        System.loadLibrary("notedeck_chrome");
    }


    private native void nativeOnKeyboardHeightChanged(int height);

    private native void nativeOnFilePickedFailed(String uri, String e);

    private native void nativeOnFilePickedWithContent(Object[] uri_info, byte[] content);

    public void openFilePicker() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.setType("*/*");
        intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        startActivityForResult(intent, REQUEST_CODE_PICK_FILE);
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

    private KeyboardHeightHelper keyboardHelper;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // Shrink view so it does not get covered by insets.

        setupInsets();
        //setupFullscreen()
        keyboardHelper = new KeyboardHeightHelper(this);

        super.onCreate(savedInstanceState);
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

    private void setupInsets() {
        View content = getContent();
        ViewCompat.setOnApplyWindowInsetsListener(content, (v, windowInsets) -> {
            Insets insets = windowInsets.getInsets(WindowInsetsCompat.Type.systemBars());

            ViewGroup.MarginLayoutParams mlp = (ViewGroup.MarginLayoutParams) v.getLayoutParams();
            mlp.topMargin = insets.top;
            mlp.leftMargin = insets.left;
            mlp.bottomMargin = insets.bottom;
            mlp.rightMargin = insets.right;
            v.setLayoutParams(mlp);

            return WindowInsetsCompat.CONSUMED;
        });

        WindowCompat.setDecorFitsSystemWindows(getWindow(), true);
    }

    @Override
    public void onResume() {
        super.onResume();
        keyboardHelper.start();
    }

    @Override
    public void onPause() {
        super.onPause();
        keyboardHelper.stop();
    }

    @Override
    public void onDestroy() {
        super.onDestroy();
        keyboardHelper.close();
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
