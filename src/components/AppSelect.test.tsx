import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AppSelect } from "./AppSelect";

describe("AppSelect", () => {
  it("filters options and commits the chosen value", () => {
    const onChange = vi.fn();
    render(
      <AppSelect
        ariaLabel="Device"
        onChange={onChange}
        options={[
          { value: "usb", label: "USB microphone" },
          { value: "internal", label: "Internal microphone" },
        ]}
        value="usb"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Device" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Device search" }), {
      target: { value: "internal" },
    });
    fireEvent.click(screen.getByRole("option", { name: "Internal microphone" }));

    expect(onChange).toHaveBeenCalledWith("internal");
  });

  it("keeps a selected option visible when the list is capped", () => {
    const options = Array.from({ length: 100 }, (_, index) => ({
      value: `device-${index}`,
      label: `Device ${index}`,
    }));
    render(
      <AppSelect
        ariaLabel="Large device list"
        onChange={() => undefined}
        options={options}
        value="device-99"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Large device list" }));

    expect(screen.getByRole("option", { name: "Device 99" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByText("Showing 80 of 100")).toBeInTheDocument();
  });

  it("supports keyboard selection without a pointer", () => {
    const onChange = vi.fn();
    render(
      <AppSelect
        ariaLabel="Keyboard device"
        onChange={onChange}
        options={[
          { value: "first", label: "First" },
          { value: "second", label: "Second" },
        ]}
        value="first"
      />,
    );

    const trigger = screen.getByRole("button", { name: "Keyboard device" });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    fireEvent.keyDown(screen.getByRole("textbox", { name: "Keyboard device search" }), {
      key: "ArrowDown",
    });
    fireEvent.keyDown(screen.getByRole("textbox", { name: "Keyboard device search" }), {
      key: "Enter",
    });

    expect(onChange).toHaveBeenCalledWith("second");
  });
});
