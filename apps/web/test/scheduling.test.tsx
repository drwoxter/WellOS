import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  appointmentNeedsConfirmation,
  buildLanes,
  hasSchedulingConsoleAccess,
  isUpcoming,
  minutesUntil,
  NO_SCHEDULING_CAPABILITIES,
  offerReasons,
  preparationText,
  rankingNotice,
  transportNextStatuses,
  weekDays,
  weekRange,
  type Appointment,
  type Offer,
  type SchedulableResource,
} from "@/lib/access";
import { OfferCard, RankingNotice } from "@/app/scheduling/shared";

function resource(id: string, name: string): SchedulableResource {
  return {
    id,
    facility_id: "f1",
    resource_type_code: "professional",
    name,
    user_id: null,
    profession_code: "physician",
    specialty_codes: ["family_medicine"],
    languages: ["es", "en"],
    accessibility_codes: [],
    capacity: 1,
    time_zone: "Europe/Madrid",
    active: true,
    metadata: null,
    version: 1,
  };
}

function offer(over: Partial<Offer> = {}): Offer {
  return {
    id: "o1",
    patient_id: "p1",
    facility_id: "f1",
    facility_name: "Main Campus",
    access_request_id: "r1",
    matcher_run_id: "m1",
    candidate_id: "c1",
    cancellation_event_id: null,
    waitlist_entry_id: null,
    status: "offered",
    service_code: "general_medicine_consultation",
    service: {
      code: "general_medicine_consultation",
      name_en: "General medicine consultation",
      name_es: "Consulta de medicina general",
    },
    modality_code: "in_person",
    starts_at: "2026-10-02T07:00:00Z",
    ends_at: "2026-10-02T07:20:00Z",
    resources: [{ resource_id: "r-ana", role: "primary", name: "Dr. Ana" }],
    score: {
      score: 71.5,
      factors: [],
      travel: null,
      reasons: ["continuity_of_care", "matches_preferred_time"],
      supportive_actions: [],
    },
    explanation: null,
    rank: 1,
    offered_to: "patient",
    offer_expires_at: null,
    hold_expires_at: null,
    appointment_id: null,
    version: 1,
    ...over,
  };
}

function appointment(over: Partial<Appointment> = {}): Appointment {
  return {
    id: "a1",
    facility_id: "f1",
    patient_id: "p1",
    service_code: "general_medicine_consultation",
    modality_code: "in_person",
    status: "confirmed",
    starts_at: "2026-10-02T07:40:00Z",
    ends_at: "2026-10-02T08:00:00Z",
    time_zone: "Europe/Madrid",
    reason: null,
    access_request_id: null,
    offer_id: null,
    matcher_run_id: null,
    candidate_id: null,
    score: null,
    primary_resource_id: "r-ana",
    resources: [{ resource_id: "r-ana", role: "primary" }],
    visit_id: "v1",
    confirmation_required: true,
    patient_confirmed_at: null,
    confirmation_due_at: "2026-10-01T07:40:00Z",
    booked_via: "staff",
    override_reason: null,
    rescheduled_from: null,
    rescheduled_to: null,
    cancellation_reason: null,
    cancellation_note: null,
    cancelled_at: null,
    fulfilled_at: null,
    no_show_at: null,
    version: 1,
    created_at: "2026-09-30T00:00:00Z",
    updated_at: "2026-09-30T00:00:00Z",
    ...over,
  };
}

describe("scheduling capabilities", () => {
  it("opens the console only for server-derived capabilities", () => {
    expect(hasSchedulingConsoleAccess(undefined)).toBe(false);
    expect(hasSchedulingConsoleAccess(NO_SCHEDULING_CAPABILITIES)).toBe(false);
    expect(
      hasSchedulingConsoleAccess({
        ...NO_SCHEDULING_CAPABILITIES,
        can_coordinate_transport: true,
      }),
    ).toBe(true);
    expect(
      hasSchedulingConsoleAccess({
        ...NO_SCHEDULING_CAPABILITIES,
        can_review_capacity: true,
      }),
    ).toBe(true);
    expect(
      hasSchedulingConsoleAccess({
        ...NO_SCHEDULING_CAPABILITIES,
        self_service: true,
      }),
    ).toBe(false);
  });
});

describe("ranking notice", () => {
  it("states deterministic ranking whenever dMind did not rank", () => {
    const base = { artifact_id: null, synthetic: null, reused: false };
    expect(
      rankingNotice("en", { ...base, mode: "deterministic", reason: null }),
    ).toMatch(/deterministic/i);
    expect(
      rankingNotice("en", {
        ...base,
        mode: "deterministic",
        reason: "disabled",
      }),
    ).toMatch(/disabled.*deterministic/i);
    expect(
      rankingNotice("en", {
        ...base,
        mode: "deterministic",
        reason: "unavailable",
      }),
    ).toMatch(/deterministic/i);
    expect(
      rankingNotice("es", {
        ...base,
        mode: "deterministic",
        reason: "quota_exceeded",
      }),
    ).toMatch(/determinista/i);
  });

  it("labels synthetic dMind ranking as synthetic", () => {
    render(
      <RankingNotice
        lang="en"
        ranking={{
          mode: "dmind",
          artifact_id: "art",
          synthetic: true,
          reused: true,
          reason: null,
        }}
      />,
    );
    const notice = screen.getByTestId("ranking-notice");
    expect(notice.className).toContain("synthetic-notice");
    expect(notice.textContent).toMatch(/synthetic/i);
    expect(notice.textContent).toMatch(/Reused governed ranking/);
  });
});

describe("OfferCard", () => {
  it("always lists the deterministic reasons, after any dMind text", () => {
    const o = offer({
      explanation: {
        artifact_id: "art",
        text: "Keeps the same family doctor.",
        cited_sources: ["c1"],
        synthetic: true,
      },
    });
    expect(offerReasons("en", o)).toEqual([
      "Keeps the same family doctor.",
      "Same care team as before",
      "Matches your preferred times",
    ]);
    render(
      <ul>
        <OfferCard lang="en" offer={o} busy={false} />
      </ul>,
    );
    expect(screen.getByText(/synthetic/i)).toBeInTheDocument();
    expect(screen.getByText("Same care team as before")).toBeInTheDocument();
    expect(screen.getByText(/Score: 71\.5/)).toBeInTheDocument();
  });

  it("offers hold/confirm/decline for a live offer and release for a hold", async () => {
    const onAction = vi.fn();
    const user = userEvent.setup();
    const { rerender } = render(
      <ul>
        <OfferCard lang="en" offer={offer()} busy={false} onAction={onAction} />
      </ul>,
    );
    await user.click(screen.getByRole("button", { name: "Hold this option" }));
    expect(onAction).toHaveBeenLastCalledWith(
      expect.objectContaining({ id: "o1" }),
      "hold",
    );
    await user.click(
      screen.getByRole("button", { name: "Confirm appointment" }),
    );
    expect(onAction).toHaveBeenLastCalledWith(expect.anything(), "accept");

    const future = new Date(Date.now() + 9 * 60_000).toISOString();
    rerender(
      <ul>
        <OfferCard
          lang="es"
          offer={offer({ status: "held", hold_expires_at: future })}
          busy={false}
          onAction={onAction}
        />
      </ul>,
    );
    expect(
      screen.getByRole("button", { name: "Liberar reserva" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/La reserva expira en \d+ min/),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Reservar temporalmente" }),
    ).toBeNull();
  });

  it("shows no actions once the offer is closed", () => {
    render(
      <ul>
        <OfferCard
          lang="en"
          offer={offer({ status: "expired" })}
          busy={false}
          onAction={vi.fn()}
        />
      </ul>,
    );
    expect(screen.queryAllByRole("button")).toHaveLength(0);
    expect(screen.getByText("Expired")).toBeInTheDocument();
  });
});

describe("agenda lanes", () => {
  it("places every occupying record in each resource lane it uses", () => {
    const lanes = buildLanes(
      [resource("r-ana", "Dr. Ana"), resource("r-room", "Room 1")],
      [
        appointment({
          id: "a1",
          resources: [
            { resource_id: "r-ana", role: "primary" },
            { resource_id: "r-room", role: "room" },
          ],
        }),
        appointment({ id: "a2", status: "cancelled" }),
        appointment({ id: "a3", status: "rescheduled" }),
      ],
      [
        offer({
          id: "o-held",
          status: "held",
          starts_at: "2026-10-02T06:00:00Z",
        }),
        offer({ id: "o-expired", status: "expired" }),
      ],
    );
    expect(lanes.map((l) => l.resource.name)).toEqual(["Dr. Ana", "Room 1"]);
    expect(lanes[0].items.map((i) => `${i.kind}:${i.id}`)).toEqual([
      "hold:o-held",
      "appointment:a1",
    ]);
    expect(lanes[1].items.map((i) => i.id)).toEqual(["a1"]);
  });

  it("builds Monday-start weeks", () => {
    expect(weekDays("2026-10-01")).toEqual([
      "2026-09-28",
      "2026-09-29",
      "2026-09-30",
      "2026-10-01",
      "2026-10-02",
      "2026-10-03",
      "2026-10-04",
    ]);
    const { from, to } = weekRange("2026-10-01");
    expect(new Date(to).getTime() - new Date(from).getTime()).toBe(
      7 * 24 * 3_600_000,
    );
  });
});

describe("appointment helpers", () => {
  it("asks for confirmation only while required and unconfirmed", () => {
    expect(appointmentNeedsConfirmation(appointment())).toBe(true);
    expect(
      appointmentNeedsConfirmation(
        appointment({ patient_confirmed_at: "2026-09-30T10:00:00Z" }),
      ),
    ).toBe(false);
    expect(
      appointmentNeedsConfirmation(appointment({ status: "cancelled" })),
    ).toBe(false);
  });

  it("treats only confirmed future appointments as upcoming", () => {
    const now = new Date("2026-10-01T00:00:00Z");
    expect(isUpcoming(appointment(), now)).toBe(true);
    expect(isUpcoming(appointment({ status: "rescheduled" }), now)).toBe(false);
    expect(isUpcoming(appointment({ status: "cancelled" }), now)).toBe(false);
    expect(isUpcoming(appointment(), new Date("2026-10-03T00:00:00Z"))).toBe(
      false,
    );
  });

  it("uses the viewer's language for preparation text with a fallback", () => {
    const service = {
      code: "x",
      name_en: "X",
      name_es: "X",
      preparation_en: "Fast for 8 hours.",
      preparation_es: null,
    };
    expect(preparationText("es", service)).toBe("Fast for 8 hours.");
    expect(preparationText("en", null)).toBeNull();
  });

  it("never returns negative minutes for expired holds", () => {
    const now = new Date("2026-10-01T00:10:00Z");
    expect(minutesUntil("2026-10-01T00:00:00Z", now)).toBe(0);
    expect(minutesUntil("2026-10-01T00:25:00Z", now)).toBe(15);
    expect(minutesUntil(null, now)).toBeNull();
  });

  it("only offers forward transport transitions", () => {
    expect(transportNextStatuses("requested")).toEqual([
      "scheduled",
      "cancelled",
      "failed",
    ]);
    expect(transportNextStatuses("picked_up")).toEqual(["completed", "failed"]);
    expect(transportNextStatuses("completed")).toEqual([]);
    expect(transportNextStatuses("cancelled")).toEqual([]);
  });
});
