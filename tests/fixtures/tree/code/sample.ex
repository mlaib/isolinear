# def ghost, do: nil
note = "defmodule Ghost do end"
defmodule Tools do
  def next(value) do
    value + 1
  end
  defp helper(value) do
    value
  end
end
def unfinished(value) do
