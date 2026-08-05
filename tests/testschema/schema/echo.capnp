@0xdb9a3c8e4f1a2b34;

# Minimal echo interface for ogurpchik's tests/benches. `stat` exists to
# have a method tests can leave unimplemented (schema-evolution behavior:
# the caller must get `unimplemented`, not a hang).

interface Echo {
  ping @0 (msg :Text) -> (reply :Text);
  stat @1 () -> (count :UInt64);
}
