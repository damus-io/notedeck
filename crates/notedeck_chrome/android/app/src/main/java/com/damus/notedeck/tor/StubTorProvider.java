package com.damus.notedeck.tor;

/**
 * Stub TorProvider implementation for builds without native Tor support.
 * All operations are no-ops and Tor is reported as unsupported.
 */
public class StubTorProvider implements TorProvider {

    @Override
    public boolean initialize(String cacheDir, String stateDir) {
        // No-op: Tor not supported in this build
        return false;
    }

    @Override
    public boolean startSocksProxy(int port) {
        // No-op: Tor not supported in this build
        return false;
    }

    @Override
    public void stop() {
        // No-op: Tor not supported in this build
    }

    @Override
    public String getSocksProxy() {
        // Tor not supported in this build
        return null;
    }

    @Override
    public boolean isInitialized() {
        // Tor not supported in this build
        return false;
    }

    @Override
    public boolean isSupported() {
        // Tor is not supported in this build
        return false;
    }

    @Override
    public void setLogCallback(TorLogCallback callback) {
        // No-op: Tor not supported in this build
    }
}
