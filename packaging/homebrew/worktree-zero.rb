# Homebrew formula for Worktree Zero.
#
# This remains HEAD-only until the first Worktree Zero release is published.
class WorktreeZero < Formula
  desc "Thin, isolated development runtimes for coding agents"
  homepage "https://github.com/lonormaly/worktree-zero"
  license "MIT"
  head "https://github.com/lonormaly/worktree-zero.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--path", ".", "--root", prefix
  end

  test do
    assert_match "worktree", shell_output("#{bin}/wt0 --help")
  end
end
