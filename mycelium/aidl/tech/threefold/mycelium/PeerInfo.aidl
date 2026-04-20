package tech.threefold.mycelium;

parcelable PeerInfo {
    String protocol;
    String address;
    String peerType;
    String connectionState;
    long rxBytes;
    long txBytes;
    long discoveredSeconds;
    /** -1 if the peer has never successfully connected. */
    long lastConnectedSeconds;
}
