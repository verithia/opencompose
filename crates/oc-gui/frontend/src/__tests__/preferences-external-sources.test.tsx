import { render, fireEvent, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import { invokeMock } from "./setup";
import { PreferencesModal } from "../components/PreferencesModal";
import { ExternalSourcesPage } from "../components/preferences/ExternalSourcesPage";

const FREESOUND_DISCONNECTED = {
  state: "needs_reauth" as const,
  expires_at: null,
  has_refresh_token: false,
  client_configured: false,
};

const FREESOUND_CONNECTED = {
  state: "connected" as const,
  expires_at: null,
  has_refresh_token: true,
  client_configured: true,
};

beforeEach(() => {
  const saved = new Set<string>();
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (command, args) => {
    const provider = String(args?.provider ?? "");
    if (command === "sample_market_key_status") {
      return saved.has(provider) ? "saved" : "missing";
    }
    if (command === "sample_market_save_key") {
      saved.add(provider);
      return { status: "ok", provider };
    }
    if (command === "sample_market_remove_key") saved.delete(provider);
    if (command === "freesound_oauth_status") return FREESOUND_DISCONNECTED;
    if (command === "freesound_oauth_begin") return FREESOUND_CONNECTED;
    if (command === "freesound_oauth_sign_out") return FREESOUND_DISCONNECTED;
  });
});

describe("External Sources", () => {
  it("opens from Preferences and saves or removes a Jamendo client ID", async () => {
    const { getByRole, getByLabelText } = render(() => <PreferencesModal open onClose={() => {}} />);
    fireEvent.click(getByRole("button", { name: "External Sources" }));
    const field = getByLabelText("Client ID", { selector: "#external-source-jamendo" }) as HTMLInputElement;
    const form = within(field.form!);
    await waitFor(() => expect(field).not.toBeDisabled());
    fireEvent.input(field, { target: { value: "local-client" } });
    fireEvent.click(form.getByRole("button", { name: "Save key" }));
    await waitFor(() => expect(form.getByText("Credential saved.")).toBeInTheDocument());
    expect(invokeMock).toHaveBeenCalledWith("sample_market_save_key", {
      provider: "jamendo", key: "local-client",
    });
    expect(field.value).toBe("");
    fireEvent.click(form.getByRole("button", { name: "Remove credential" }));
    await waitFor(() => expect(form.getByText("No credential configured.")).toBeInTheDocument());
    expect(invokeMock).toHaveBeenCalledWith("sample_market_remove_key", { provider: "jamendo" });
  });

  it("connects and disconnects Freesound from Preferences", async () => {
    const { container, getByText } = render(() => <ExternalSourcesPage />);
    await waitFor(() => expect(getByText("Connect Freesound")).toBeInTheDocument());
    expect(getByText("Pulls use the 128 kbps preview until you connect.")).toBeInTheDocument();

    const inputs = container.querySelectorAll(".sample-market-oauth input");
    expect(inputs).toHaveLength(2);
    fireEvent.input(inputs[0], { target: { value: "client-id" } });
    fireEvent.input(inputs[1], { target: { value: "client-secret" } });
    fireEvent.click(getByText("Connect Freesound"));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("freesound_oauth_begin", {
        clientId: "client-id",
        clientSecret: "client-secret",
      }),
    );
    await waitFor(() =>
      expect(getByText("Connected. Pulls download the original file.")).toBeInTheDocument(),
    );

    fireEvent.click(getByText("Disconnect"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("freesound_oauth_sign_out"),
    );
    await waitFor(() =>
      expect(getByText("Pulls use the 128 kbps preview until you connect.")).toBeInTheDocument(),
    );
  });
});
