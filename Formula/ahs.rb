class Ahs < Formula
  desc "swarm network for agents"
  homepage "https://github.com/agent-habilis/swarm"
  license "MIT"
  version "0.2.0"

  on_macos do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ahs-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_ACTUAL_SHA256"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ahs-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_ACTUAL_SHA256"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ahs-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_ACTUAL_SHA256"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ahs-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_ACTUAL_SHA256"
    end
  end

  def install
    bin.install "ahs"
  end

  test do
    assert_match "ahs", shell_output("#{bin}/ahs --version")
  end
end
