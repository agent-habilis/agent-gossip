class AgentHabilisSwarm < Formula
  desc "P2P swarm network for AI agents"
  homepage "https://github.com/agent-habilis/agent-habilis-swarm"
  license "MIT"

  on_macos do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/agent-habilis-swarm/releases/download/v#{version}/agent-habilis-swarm-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_ACTUAL_SHA256"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-habilis-swarm/releases/download/v#{version}/agent-habilis-swarm-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_ACTUAL_SHA256"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/agent-habilis-swarm/releases/download/v#{version}/agent-habilis-swarm-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_ACTUAL_SHA256"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-habilis-swarm/releases/download/v#{version}/agent-habilis-swarm-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_ACTUAL_SHA256"
    end
  end

  def install
    bin.install "agent-habilis-swarm"
  end

  test do
    assert_match "agent-habilis-swarm", shell_output("#{bin}/agent-habilis-swarm --version")
  end
end
