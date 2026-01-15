package com.damus.notedeck.tor;

import android.util.Log;

/**
 * TorProvider implementation using the native Arti library.
 */
public class NativeTorProvider implements TorProvider {
    private static final String TAG = "NativeTorProvider";
    private int socksPort = -1;

    @Override
    public boolean initialize(String cacheDir, String stateDir) {
        if (!ArtiNative.isLibraryLoaded()) {
            Log.e(TAG, "Native library not loaded: " + ArtiNative.getLoadError());
            return false;
        }

        try {
            return ArtiNative.initialize(cacheDir, stateDir);
        } catch (Exception e) {
            Log.e(TAG, "Initialize failed", e);
            return false;
        }
    }

    @Override
    public boolean startSocksProxy(int port) {
        if (!ArtiNative.isLibraryLoaded()) {
            Log.e(TAG, "Native library not loaded");
            return false;
        }

        try {
            boolean result = ArtiNative.startSocksProxy(port);
            if (result) {
                socksPort = port;
            }
            return result;
        } catch (Exception e) {
            Log.e(TAG, "startSocksProxy failed", e);
            return false;
        }
    }

    @Override
    public void stop() {
        if (!ArtiNative.isLibraryLoaded()) {
            return;
        }

        try {
            ArtiNative.stop();
            socksPort = -1;
        } catch (Exception e) {
            Log.e(TAG, "stop failed", e);
        }
    }

    @Override
    public String getSocksProxy() {
        if (!ArtiNative.isLibraryLoaded()) {
            return null;
        }

        try {
            int port = ArtiNative.getSocksPort();
            if (port > 0) {
                return "127.0.0.1:" + port;
            }
        } catch (Exception e) {
            Log.e(TAG, "getSocksProxy failed", e);
        }
        return null;
    }

    @Override
    public boolean isInitialized() {
        if (!ArtiNative.isLibraryLoaded()) {
            return false;
        }

        try {
            return ArtiNative.isInitialized();
        } catch (Exception e) {
            Log.e(TAG, "isInitialized failed", e);
            return false;
        }
    }

    @Override
    public boolean isSupported() {
        return ArtiNative.isLibraryLoaded();
    }

    @Override
    public void setLogCallback(TorLogCallback callback) {
        if (!ArtiNative.isLibraryLoaded()) {
            return;
        }

        try {
            ArtiNative.setLogCallback(callback);
        } catch (Exception e) {
            Log.e(TAG, "setLogCallback failed", e);
        }
    }
}
