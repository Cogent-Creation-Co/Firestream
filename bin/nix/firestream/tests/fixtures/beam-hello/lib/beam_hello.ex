defmodule BeamHello.Application do
  @moduledoc """
  The fixture for mkBeamContainerModule: a release that answers `ok` on a TCP
  port, so a test can prove the release boots and serves.
  """
  use Application

  @impl true
  def start(_type, _args) do
    port = String.to_integer(System.get_env("BEAM_HELLO_PORT", "4000"))
    children = [{Task, fn -> BeamHello.Server.listen(port) end}]
    Supervisor.start_link(children, strategy: :one_for_one)
  end
end

defmodule BeamHello.Server do
  @moduledoc "Accepts TCP connections and answers each with `ok`."
  require Logger

  def listen(port) do
    {:ok, socket} = :gen_tcp.listen(port, [:binary, active: false, reuseaddr: true])
    Logger.info("beam_hello listening on #{port}")
    accept(socket)
  end

  defp accept(socket) do
    {:ok, client} = :gen_tcp.accept(socket)
    :gen_tcp.send(client, "ok\n")
    :gen_tcp.close(client)
    accept(socket)
  end
end

defmodule BeamHello.Release do
  @moduledoc "Release tasks, the shape that `preStart` calls with `eval`."

  def migrate do
    IO.puts("beam_hello migrate ran")
  end
end
