class AgentGossip < Formula
  desc "mesh network for agents"
  homepage "https://github.com/agent-habilis/agent-gossip"
  license "MIT"
  version "0.7.3"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "49a8feca77e51278954b9aafd80188fc7d70f86f20d45b2ddec931a7f8c3395a"
    else
      # The digest below is a placeholder until the first release that builds
      # this target; the release workflow rewrites it. Nothing may come
      # between the `url` and `sha256` lines — the rewrite matches the digest
      # only when it directly follows the url, so a comment wedged in there
      # leaves the placeholder in place and ships a formula that cannot
      # verify. Keep it 64 hex chars for the same reason.
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "c873fbaafd17f41fed4e5b2002633eca234259313c3ffe5fd1d97035347eb4c7"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "e1f299b899b412a7f5e439d0b51297857e4c6ccee21832b4bab932feb11ab49b"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "146255e81ff791659f655f061c25ea1b05208de044df59b51afb13c773f22b6f"
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
