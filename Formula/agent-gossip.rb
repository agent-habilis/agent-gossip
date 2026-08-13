class AgentGossip < Formula
  desc "mesh network for agents"
  homepage "https://github.com/agent-habilis/agent-gossip"
  license "MIT"
  version "0.8.0"

  # The release workflow rewrites every version and digest below, matching a
  # `sha256` line only where it directly follows its `url`. Nothing may be put
  # between the two, or that pair keeps its stale digest through the bump and
  # ships a formula that cannot verify.
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "c579dacdc593cb05b3bafbbb92d5d8401a318ac4f4a3f0e908b308a75db13198"
    else
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "5c871e24d77cf06b60df315e6ff7bdf273f0417bf5d00d0c887a56b6e6dda226"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "c623a0f38caa797f0650163af6aae91f3c333298809130ccfd63484fc6405782"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "48653d26fc8ea265850bf49a3ee559bbf2a55c1f2650d978d6494441c6ded77f"
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
