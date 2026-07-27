class AgentGossip < Formula
  desc "mesh network for agents"
  homepage "https://github.com/agent-habilis/agent-gossip"
  license "MIT"
  version "0.7.2"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "936f57d456bd5cd9425c1eefa74fc1930b4570320d654b217c2fd40c6cdc5ece"
    else
      # The digest below is a placeholder until the first release that builds
      # this target; the release workflow rewrites it. Nothing may come
      # between the `url` and `sha256` lines — the rewrite matches the digest
      # only when it directly follows the url, so a comment wedged in there
      # leaves the placeholder in place and ships a formula that cannot
      # verify. Keep it 64 hex chars for the same reason.
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "c0eb20cd8635ab64dc595cfde100e157067b8bfd1f231afbfd12bb34abb17a25"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "6b92828cb18ca0ceab994f0482954e2743f97c987d550ef5a4ead56c1c04f3ee"
    end
  end

  def install
    bin.install "agent-gossip"
    man1.install Dir["man/*.1"]
  end

  test do
    assert_match "agent-gossip", shell_output("#{bin}/agent-gossip --version")
  end
end
