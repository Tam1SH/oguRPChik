@0xb7e2d1c4f5a69308;

# Contract between the host and out-of-process plugins (npipe on Windows,
# uds on Linux).
#
# Security model: after the handshake attests the plugin process, the
# bootstrap capability *is* the permission boundary — the plugin can only
# call what is reachable from PluginHost, not "the whole API because its
# PID checked out". Grant narrower interfaces per plugin as the API grows.

# Implemented by the plugin, called by the host.
interface Plugin {
  # A command routed from the UI through the host to the plugin.
  onCommand @0 (name :Text, args :List(Prop)) -> (result :Text);
}

# Implemented by the plugin; the host pushes state changes into it.
# (`-> stream` is push-style: payload in the parameters, the returned
# promise carries flow-control backpressure.)
interface StateSink {
  push @0 (value :Text) -> stream;
}

# Implemented by the host, exported to the plugin as its bootstrap
# capability.
interface PluginHost {
  queryState @0 (path :Text) -> (value :Text);

  # Register the plugin's sink; the host pushes state changes for `path`.
  subscribeState @1 (path :Text, sink :StateSink) -> ();
}

# Cap'n Proto has no map type; key/value lists are the idiom.
struct Prop {
  key   @0 :Text;
  value @1 :Text;
}
