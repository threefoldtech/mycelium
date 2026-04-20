package tech.threefold.mycelium;

parcelable QueriedSubnet {
    String subnet;
    /** Seconds until this query expires. */
    long expirationSeconds;
}
