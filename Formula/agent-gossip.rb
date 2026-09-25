class AgentGossip < Formula
  desc "mesh network for agents"
  homepage "https://github.com/agent-habilis/agent-gossip"
  license "MIT"
  version "0.11.1"

  # The release workflow rewrites every version and digest below, matching a
  # `sha256` line only where it directly follows its `url`. Nothing may be put
  # between the two, or that pair keeps its stale digest through the bump and
  # ships a formula that cannot verify.
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "53232839ae39afd7a067e5bf873f4a66ab05783243b2686463a071c42f7cb0c6"
    else
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "e5349adcddefcda28ad32cb8424e0889f0d9cc181b0f1ca27093f69929d9c2f4"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "15697bb9f9a662d50763b2854e81d4d511dfd6559e091f7ccf7327d21a6d7140"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5f380443ddb5342b4cf660dc947b5a98b022b90f39d30b4207edf3ad10f79dc7"
    end
  end

  def install
    bin.install "agent-gossip"
    man1.install Dir["man/*.1"]
  end

  # `plug` cannot run from `install` or `post_install`: both are sandboxed with
  # `deny_read_home` and no write path into `$HOME`, so it could not even detect
  # which harnesses are on the machine. It stays the user's step.
  def caveats
    <<~EOS
      Install the skills into every harness detected on this machine:
      ! agent-gossip plug

      Then check the setup with:
      ! agent-gossip doctor
    EOS
  end

  test do
    assert_match "agent-gossip", shell_output("#{bin}/agent-gossip --version")
  end
end
