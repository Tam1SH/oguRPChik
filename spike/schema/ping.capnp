@0x9eb32e19f86ee174;

interface Ping {
    # Symmetric interface: both host and agent implement this and call each other
    # over the same bootstrap capability.
    ping @0 (msg :Text) -> (reply :Text);

    # Streaming method, as would be used for metrics/UI-diff delivery.
    # capnp-rpc handles flow control for -> stream methods automatically.
    pushSample @1 (value :UInt64) -> stream;
    done @2 () -> ();
}
