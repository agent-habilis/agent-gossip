class Ahs < Formula
  desc "swarm network for agents"
  homepage "https://github.com/agent-habilis/swarm"
  license "MIT"
  version "0.4.2"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ahs-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "fed65aa6b65bd76dd6bf10d193c7850bb20416fe0ee29d47c7f52bee06c476d6"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ahs-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "3fccc35b33d954d4b5ea6835385668fc2fc2d5bdaac6e0e3dbcfcfbc71adafe3"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ahs-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "36876bfe415040fa7aa1d06e7e17c96e109a84e80a48bbfe4c954d915e78c203"
    end
  end

  def install
    bin.install "ahs"
  end

  test do
    assert_match "ahs", shell_output("#{bin}/ahs --version")
  end
end
