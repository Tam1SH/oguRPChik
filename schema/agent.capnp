@0xa3f1c0d2e4b59687;

# Contract between the host and an in-VM agent (vsock).
#
# capnp twoparty connections are symmetric: each side exports one bootstrap
# capability and may call the other's over the same connection. Evolution
# rules: new fields/methods get new ordinals, old ordinals are never reused,
# unknown fields/methods are ignored by the reader (a missing method answers
# `unimplemented` to the caller).
#
# Streaming note: `-> stream` methods are push-style — the payload goes in
# the *parameters* and the method returns Promise<void>, whose resolution
# doubles as flow-control backpressure (capnp-rpc delivers one call at a
# time). A "subscription" is therefore a callback capability with a
# streaming push method, registered once via a regular call.

# Implemented by the host, called by the agent.
interface HostEvents {
  # Agent -> host: a UI/state event (a diff) from inside the VM.
  pushEvent @0 (kind :Text, payload :Data) -> ();
}

# Implemented by the host; the agent pushes metric samples into it.
interface MetricSink {
  push @0 (sample :MetricSample) -> stream;
}

# Implemented by the agent, called by the host.
interface AgentControl {
  ping @0 (msg :Text) -> (reply :Text);

  # Register the host's metric sink; the agent starts pushing samples.
  subscribeMetrics @1 (sink :MetricSink) -> ();

  # The host hands its event sink to the agent through this call.
  getHostEvents @2 () -> (events :HostEvents);
}

struct MetricSample {
  timestampMs @0 :UInt64;
  cpuPercent  @1 :Float64;
  memBytes    @2 :UInt64;
}
