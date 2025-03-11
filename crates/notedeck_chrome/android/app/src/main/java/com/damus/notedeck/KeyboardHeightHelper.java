package com.damus.notedeck;

import android.app.Activity;
import android.content.res.Configuration;
import android.util.Log;
import android.view.View;

public class KeyboardHeightHelper {
    private static final String TAG = "KeyboardHeightHelper";
    private KeyboardHeightProvider keyboardHeightProvider;
    private Activity activity;

    // Static JNI method not tied to any specific activity
    private static native void nativeKeyboardHeightChanged(int height);

    public KeyboardHeightHelper(Activity activity) {
        this.activity = activity;
        keyboardHeightProvider = new KeyboardHeightProvider(activity);
        
        // Create observer implementation
        KeyboardHeightObserver observer = (height, orientation) -> {
            Log.d(TAG, "Keyboard height: " + height + "px, orientation: " + 
                 (orientation == Configuration.ORIENTATION_PORTRAIT ? "portrait" : "landscape"));
            
            // Call the generic native method
            nativeKeyboardHeightChanged(height);
        };
        
        // Set up the provider
        keyboardHeightProvider.setKeyboardHeightObserver(observer);
    }
    
    public void start() {
        // Start the keyboard height provider after the view is ready
        final View contentView = activity.findViewById(android.R.id.content);
        contentView.post(() -> {
            keyboardHeightProvider.start();
        });
    }
    
    public void stop() {
        keyboardHeightProvider.setKeyboardHeightObserver(null);
    }
    
    public void close() {
        keyboardHeightProvider.close();
    }
}
