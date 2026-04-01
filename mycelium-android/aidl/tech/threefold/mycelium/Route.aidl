package tech.threefold.mycelium;

parcelable Route {
    String subnet;
    String nextHop;
    /** Computed metric. -1 means infinite (unreachable). */
    long metric;
    int seqno;
}
