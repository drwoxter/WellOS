"use client";

import Link from "next/link";
import { useEffect, useMemo, useState } from "react";
import { AppShell } from "../chrome";
import { t, type TKey } from "@/lib/i18n";
import { apiFetch, useSession } from "@/lib/session";
import { formatDate, formatDateTime } from "@/lib/clinical";
import {
  appointmentNeedsConfirmation,
  appointmentStatusLabel,
  catalogName,
  preparationText,
  requestStatusLabel,
  type Appointment,
  type Me,
} from "@/lib/access";
import {
  criticalityLabel,
  criticalityTone,
  explanationFor,
  orderStatusLabel,
  preparationFor,
  type MyReleasedResult,
} from "@/lib/diagnostics";
import {
  greetingKey,
  isNewRelease,
  latestExplained,
  loadPatientHome,
  referenceBand,
  trendPoints,
  type HomeNextAction,
  type PatientHome,
} from "@/lib/home";
import { postJson, useAction, useLoader } from "../scheduling/shared";
import { patientAccessApi } from "../scheduling/find-appointment";
import {
  Card,
  CardHead,
  EmptyState,
  ErrorState,
  ListRow,
  Pill,
  Skeleton,
  Timeline,
  type TimelineItem,
  type Tone,
} from "@/components/ui/primitives";
import { TrendChart } from "@/components/ui/charts";
import { DmindPresence } from "@/components/ui/dmind";
import { Icon } from "@/components/ui/icons";
import { useDmindPageContext } from "@/lib/dmind-context";

const ME = "/api/v1/me";

function actionCopy(
  a: HomeNextAction,
): { text: TKey; cta: TKey; href: string } | null {
  switch (a.kind) {
    case "confirm_attendance":
      return {
        text: "homeActionConfirm",
        cta: "confirmAttendance",
        href: "/my/appointments",
      };
    case "review_options":
      return {
        text: "homeActionOptions",
        cta: "homeReviewOptions",
        href: "/my/appointments?tab=requests",
      };
    case "new_results":
      return {
        text: "homeActionNewResults",
        cta: "homeSeeResults",
        href: "/my/diagnostics",
      };
    case "prepare_appointment":
      return {
        text: "homeActionPrepare",
        cta: "homeSeeAppointment",
        href: "/my/appointments",
      };
    case "read_notifications":
      return {
        text: "homeActionNotifications",
        cta: "homeSeeMessages",
        href: "/my/appointments?tab=notifications",
      };
    case "none":
      return null;
  }
}

function requestStatusCopy(status: string): TKey | null {
  switch (status) {
    case "needs_clinical_triage":
      return "homeRequestStatusTriage";
    case "submitted":
      return "homeRequestStatusSubmitted";
    case "options_ready":
      return "homeRequestStatusOptions";
    case "draft":
      return "homeRequestStatusDraft";
    default:
      return null;
  }
}

export default function PatientHomePage() {
  const { lang, meta, authenticated } = useSession();
  const selfService =
    (meta?.scheduling_capabilities?.self_service ?? false) ||
    (meta?.diagnostics_capabilities?.self_service ?? false);
  const me = useLoader(
    () => apiFetch<Me>(ME),
    "me",
    authenticated === true && selfService,
  );
  const [patientId, setPatientId] = useState<string | null>(null);
  const patients = useMemo(() => me.data?.patients ?? [], [me.data]);
  useEffect(() => {
    if (!patientId && patients.length > 0) setPatientId(patients[0].patient_id);
    if (patientId && !patients.some((p) => p.patient_id === patientId)) {
      setPatientId(patients[0]?.patient_id ?? null);
    }
  }, [patients, patientId]);
  const home = useLoader(
    () => loadPatientHome(patientId ?? undefined),
    `${patientId}`,
    authenticated === true && selfService && patientId !== null,
  );
  // Only the selected person's home is ever shown: while a switch is in
  // flight the previous person's data stays out of the page.
  const data =
    home.data && home.data.patient_id === patientId ? home.data : null;
  const { busy, message, run } = useAction(lang);
  const L = (k: TKey) => t(lang, k);

  const explained = useMemo(
    () => (data ? latestExplained(data.diagnostics.released) : null),
    [data],
  );
  useDmindPageContext(
    data
      ? {
          state: explained ? "ready" : "idle",
          items: explained
            ? [
                {
                  id: explained.id,
                  title: explained.order_display,
                  sub: L("homeExplanationSource"),
                  href: "/my/diagnostics",
                  status: "approved",
                  created_at: explained.released_at,
                  tone: "dmind",
                },
              ]
            : [],
          note: L("homeExplanationSub"),
        }
      : null,
  );

  const hour = new Date().getHours();
  const current = patients.find((p) => p.patient_id === patientId);
  const isSelf = current?.relationship === "self";

  return (
    <AppShell>
      <div className="patient-home" data-testid="patient-home">
        {authenticated === null ||
        (authenticated && selfService && me.loading) ? (
          <Skeleton title lines={4} label={L("loading")} />
        ) : !authenticated ? (
          <p className="advisory" role="status">
            {L("signInRequired")}
          </p>
        ) : !selfService || me.denied ? (
          <EmptyState
            title={L("notAuthorized")}
            description={L("selfServiceUnauthorized")}
            icon={<Icon.Shield />}
          />
        ) : me.error ? (
          <ErrorState
            title={L("stateErrorTitle")}
            description={me.error}
            onRetry={me.reload}
            retryLabel={L("retry")}
          />
        ) : patients.length === 0 ? (
          <EmptyState title={L("noPatientGrants")} icon={<Icon.User />} />
        ) : (
          <>
            <section
              className="hero patient-hero"
              aria-labelledby="home-greeting"
            >
              <p className="eyebrow">
                {formatDate(lang, new Date().toISOString())}
              </p>
              <h1 id="home-greeting">
                {L(greetingKey(hour))}
                {isSelf && current ? `, ${current.patient.given_name}` : ""}
              </h1>
              {!isSelf && current ? (
                <p className="hero-sub">
                  {L("homeOnBehalf")} {current.patient.given_name}{" "}
                  {current.patient.family_name}
                </p>
              ) : null}
              {patients.length > 1 ? (
                <div
                  className="chips"
                  role="group"
                  aria-label={L("homeChoosePerson")}
                >
                  {patients.map((p) => (
                    <button
                      key={p.patient_id}
                      type="button"
                      className="chip"
                      aria-pressed={p.patient_id === patientId}
                      onClick={() => setPatientId(p.patient_id)}
                    >
                      {p.patient_id === patientId ? <Icon.Check /> : null}
                      {p.patient.given_name} {p.patient.family_name}
                    </button>
                  ))}
                </div>
              ) : null}
            </section>

            {message ? (
              <p
                className={
                  message.kind === "error" ? "advisory error" : "advisory ok"
                }
                role="status"
              >
                {message.text}
              </p>
            ) : null}

            {data ? (
              <HomeBody
                lang={lang}
                home={data}
                explained={explained}
                busy={busy}
                onConfirm={(ap) =>
                  void run(async () => {
                    await postJson(`${ME}/appointments/${ap.id}/confirm`, {
                      version: ap.version,
                    });
                    home.reload();
                  }, "attendanceConfirmed")
                }
              />
            ) : home.error ? (
              <ErrorState
                title={L("stateErrorTitle")}
                description={home.error}
                onRetry={home.reload}
                retryLabel={L("retry")}
              />
            ) : (
              <Skeleton title lines={6} label={L("loading")} />
            )}
          </>
        )}
      </div>
    </AppShell>
  );
}

function HomeBody({
  lang,
  home,
  explained,
  busy,
  onConfirm,
}: {
  lang: "en" | "es";
  home: PatientHome;
  explained: MyReleasedResult | null;
  busy: boolean;
  onConfirm: (a: Appointment) => void;
}) {
  const L = (k: TKey) => t(lang, k);
  const next = home.appointments.upcoming[0] ?? null;
  const action = actionCopy(home.next_action);
  const nextAction = home.next_action;
  const actionAppt =
    nextAction.kind === "confirm_attendance"
      ? home.appointments.upcoming.find(
          (a) => a.id === nextAction.appointment_id,
        )
      : undefined;
  const released = home.diagnostics.released;
  const newReleased = released.filter((r) => isNewRelease(r));

  const tasks: { id: string; title: string; href: string; tone: Tone }[] = [];
  home.appointments.upcoming.filter(appointmentNeedsConfirmation).forEach((a) =>
    tasks.push({
      id: `confirm-${a.id}`,
      title: `${L("homeTaskConfirm")} · ${formatDateTime(lang, a.starts_at)}`,
      href: "/my/appointments",
      tone: "warn",
    }),
  );
  home.requests
    .filter((r) => r.status === "options_ready")
    .forEach((r) =>
      tasks.push({
        id: `options-${r.id}`,
        title: L("homeTaskOptions"),
        href: "/my/appointments?tab=requests",
        tone: "teal",
      }),
    );
  newReleased.forEach((r) =>
    tasks.push({
      id: `result-${r.id}`,
      title: `${L("homeTaskResult")} · ${r.order_display}`,
      href: "/my/diagnostics",
      tone: "dmind",
    }),
  );
  if (home.counts.unread_notifications > 0) {
    tasks.push({
      id: "messages",
      title: `${L("homeTaskMessage")} (${home.counts.unread_notifications})`,
      href: "/my/appointments?tab=notifications",
      tone: "neutral",
    });
  }

  const timeline: (TimelineItem & { at: string })[] = [
    ...home.appointments.recent.map((a) => ({
      id: `a-${a.id}`,
      at: a.starts_at,
      when: formatDateTime(lang, a.starts_at),
      title: catalogName(lang, a.service, a.service_code),
      sub: `${appointmentStatusLabel(lang, a.status)}${
        a.facility_name ? ` · ${a.facility_name}` : ""
      }`,
      tone: (a.status === "fulfilled"
        ? "ok"
        : a.status === "cancelled" || a.status === "no_show"
          ? "neutral"
          : "teal") as Tone,
      href: "/my/appointments",
    })),
    ...released.map((r) => ({
      id: `r-${r.id}`,
      at: r.released_at,
      when: formatDateTime(lang, r.released_at),
      title: r.order_display,
      sub: criticalityLabel(lang, r.criticality),
      tone: criticalityTone(r.criticality) as Tone,
      href: "/my/diagnostics",
    })),
  ]
    .sort((a, b) => b.at.localeCompare(a.at))
    .slice(0, 8);

  return (
    <>
      <Card
        className="home-next"
        tone={action ? "teal" : "ok"}
        aria-labelledby="home-next-title"
      >
        <CardHead
          id="home-next-title"
          eyebrow={L("homeWhatMatters")}
          title={action ? L("homeNextAction") : L("homeNothingPending")}
        />
        {action ? (
          <>
            <p className="home-next-text">{L(action.text)}</p>
            {actionAppt ? (
              <p className="muted">
                {catalogName(lang, actionAppt.service, actionAppt.service_code)}{" "}
                · {formatDateTime(lang, actionAppt.starts_at)}
                {actionAppt.facility_name
                  ? ` · ${actionAppt.facility_name}`
                  : ""}
              </p>
            ) : null}
            <div className="actions wrap">
              {actionAppt ? (
                <button
                  type="button"
                  className="primary"
                  disabled={busy}
                  onClick={() => onConfirm(actionAppt)}
                  data-testid="home-confirm-attendance"
                >
                  <Icon.Check /> {L("confirmAttendance")}
                </button>
              ) : (
                <Link className="button primary" href={action.href}>
                  {L(action.cta)} <Icon.ArrowRight />
                </Link>
              )}
            </div>
          </>
        ) : (
          <p className="muted">{L("homeNothingPendingSub")}</p>
        )}
      </Card>

      <div className="bento home-grid">
        <Card className="span-6" aria-labelledby="home-appt-title">
          <CardHead
            id="home-appt-title"
            title={L("homeNextAppointment")}
            actions={
              <Link className="button secondary" href="/my/appointments">
                {L("homeAllAppointments")}
              </Link>
            }
          />
          {next ? (
            <div className="home-appt">
              <p className="home-appt-when">
                <Icon.Calendar /> {formatDateTime(lang, next.starts_at)}
              </p>
              <p className="home-appt-what">
                {catalogName(lang, next.service, next.service_code)}
              </p>
              <p className="muted">
                {next.facility_name ?? ""}
                {next.primary_resource
                  ? ` · ${next.primary_resource.name}`
                  : ""}
              </p>
              {preparationText(lang, next.service) ? (
                <p className="home-prep">
                  <strong>{L("preparation")}:</strong>{" "}
                  {preparationText(lang, next.service)}
                </p>
              ) : null}
              <div className="actions wrap">
                {appointmentNeedsConfirmation(next) ? (
                  <Pill tone="warn">{L("homeConfirmationNeeded")}</Pill>
                ) : next.patient_confirmed_at ? (
                  <Pill tone="ok">{L("attendanceConfirmed")}</Pill>
                ) : null}
                <a
                  className="button secondary"
                  href={patientAccessApi.icsPath(next.id, lang)}
                  download
                >
                  {L("addToCalendar")}
                </a>
              </div>
            </div>
          ) : (
            <EmptyState
              inline
              title={L("homeNoUpcoming")}
              description={L("homeNoUpcomingSub")}
              icon={<Icon.Calendar />}
              action={
                <Link
                  className="button primary"
                  href="/my/appointments?tab=find"
                >
                  {L("homeRequestAppointment")}
                </Link>
              }
            />
          )}
          {home.requests.length > 0 ? (
            <ul
              className="list-plain home-requests"
              aria-label={L("homeRequestsOpen")}
            >
              {home.requests.map((r) => {
                const copy = requestStatusCopy(r.status);
                return (
                  <ListRow
                    key={r.id}
                    title={copy ? L(copy) : requestStatusLabel(lang, r.status)}
                    sub={r.free_text ?? formatDateTime(lang, r.created_at)}
                    tone={r.status === "options_ready" ? "teal" : "neutral"}
                    actions={
                      <Link
                        className="button secondary"
                        href="/my/appointments?tab=requests"
                      >
                        {L("homeOpen")}
                      </Link>
                    }
                  />
                );
              })}
            </ul>
          ) : null}
        </Card>

        <Card className="span-6" aria-labelledby="home-results-title">
          <CardHead
            id="home-results-title"
            title={L("homeResults")}
            actions={
              <Link className="button secondary" href="/my/diagnostics">
                {L("homeSeeResults")}
              </Link>
            }
          />
          {released.length === 0 &&
          home.counts.under_review === 0 &&
          home.diagnostics.pending.length === 0 ? (
            <EmptyState
              inline
              title={L("homeNoResults")}
              description={L("homeNoResultsSub")}
              icon={<Icon.Flask />}
            />
          ) : (
            <>
              {released.length > 0 ? (
                <ul className="list-plain" aria-label={L("homeResults")}>
                  {released.slice(0, 4).map((r) => (
                    <ListRow
                      key={r.id}
                      title={
                        <>
                          {r.order_display}{" "}
                          {isNewRelease(r) ? (
                            <Pill tone="teal" icon={false}>
                              {L("homeNewResult")}
                            </Pill>
                          ) : null}
                        </>
                      }
                      sub={`${formatDate(lang, r.released_at)}${
                        r.conclusion ? ` · ${r.conclusion}` : ""
                      }`}
                      tone={criticalityTone(r.criticality) as Tone}
                      leading={
                        <Pill tone={criticalityTone(r.criticality) as Tone}>
                          {criticalityLabel(lang, r.criticality)}
                        </Pill>
                      }
                    />
                  ))}
                </ul>
              ) : null}
              {home.counts.under_review > 0 ? (
                <p className="home-under-review">
                  <Icon.Clock /> {home.counts.under_review}{" "}
                  {L("homeUnderReviewCount")}
                  <span className="muted"> — {L("homeUnderReviewSub")}</span>
                </p>
              ) : null}
              {home.diagnostics.pending.length > 0 ? (
                <div className="home-pending">
                  <h3>{L("homePendingOrders")}</h3>
                  <ul className="list-plain">
                    {home.diagnostics.pending.slice(0, 3).map((o) => (
                      <ListRow
                        key={o.service_request_id}
                        title={o.order_display}
                        sub={
                          <>
                            {orderStatusLabel(lang, o.status)}
                            {o.starts_at
                              ? ` · ${formatDateTime(lang, o.starts_at)}`
                              : ""}
                            {preparationFor(lang, o) ? (
                              <>
                                <br />
                                <strong>{L("preparation")}:</strong>{" "}
                                {preparationFor(lang, o)}
                              </>
                            ) : null}
                          </>
                        }
                      />
                    ))}
                  </ul>
                </div>
              ) : null}
            </>
          )}
        </Card>

        <Card className="span-4" aria-labelledby="home-tasks-title">
          <CardHead id="home-tasks-title" title={L("homeTasks")} />
          {tasks.length === 0 ? (
            <EmptyState
              inline
              title={L("homeNoTasks")}
              description={L("homeNoTasksSub")}
              icon={<Icon.Tasks />}
            />
          ) : (
            <ul className="list-plain" aria-label={L("homeTasks")}>
              {tasks.map((task) => (
                <ListRow
                  key={task.id}
                  title={task.title}
                  tone={task.tone}
                  actions={
                    <Link className="button secondary" href={task.href}>
                      {L("homeOpen")}
                    </Link>
                  }
                />
              ))}
            </ul>
          )}
        </Card>

        <Card className="span-4" aria-labelledby="home-messages-title">
          <CardHead
            id="home-messages-title"
            title={L("homeMessages")}
            eyebrow={
              home.counts.unread_notifications > 0
                ? `${home.counts.unread_notifications} ${L("homeUnreadCount")}`
                : undefined
            }
            actions={
              <Link
                className="button secondary"
                href="/my/appointments?tab=notifications"
              >
                {L("homeSeeMessages")}
              </Link>
            }
          />
          {home.notifications.length === 0 ? (
            <EmptyState
              inline
              title={L("homeNoMessages")}
              description={L("homeNoMessagesSub")}
              icon={<Icon.Bell />}
            />
          ) : (
            <ul className="list-plain" aria-label={L("homeMessages")}>
              {home.notifications.map((n) => (
                <ListRow
                  key={n.id}
                  title={n.subject}
                  sub={
                    n.delivered_at ? formatDateTime(lang, n.delivered_at) : null
                  }
                  tone={n.read_at ? "neutral" : "teal"}
                />
              ))}
            </ul>
          )}
        </Card>

        <Card
          className="span-4"
          tone="dmind"
          aria-labelledby="home-explain-title"
        >
          <CardHead
            id="home-explain-title"
            title={L("homeExplanation")}
            eyebrow={
              <DmindPresence state={explained ? "ready" : "idle"} lang={lang} />
            }
          />
          {explained ? (
            <>
              <p className="home-explain-what">
                <strong>{explained.order_display}</strong> ·{" "}
                {formatDate(lang, explained.released_at)}
              </p>
              <p>{explanationFor(lang, explained)}</p>
              <p className="muted small">
                {L("homeExplanationSource")}. {L("homeExplanationSub")}
              </p>
            </>
          ) : (
            <EmptyState
              inline
              title={L("homeNoResults")}
              description={L("homeExplanationSub")}
              icon={<Icon.Sparkle />}
            />
          )}
        </Card>

        <Card className="span-7" aria-labelledby="home-trends-title">
          <CardHead
            id="home-trends-title"
            title={L("homeTrends")}
            eyebrow={L("homeTrendsSub")}
          />
          {home.trends.length === 0 ? (
            <EmptyState
              inline
              title={L("homeNoTrends")}
              description={L("homeNoTrendsSub")}
              icon={<Icon.Pulse />}
            />
          ) : (
            <div className="home-trends">
              {home.trends.slice(0, 3).map((tr) => {
                const pts = trendPoints(lang, tr);
                const last = tr.points[tr.points.length - 1];
                const title = tr.display ?? tr.code;
                return (
                  <TrendChart
                    key={`${tr.code}-${tr.unit}`}
                    title={title}
                    unit={tr.unit}
                    points={pts}
                    band={referenceBand(last?.reference_range)}
                    summary={`${title}: ${pts.length} · ${
                      last
                        ? `${last.value} ${tr.unit} (${criticalityLabel(lang, last.interpretation)}, ${formatDate(lang, last.effective_at)})`
                        : ""
                    }`}
                  />
                );
              })}
            </div>
          )}
        </Card>

        <Card className="span-5" aria-labelledby="home-timeline-title">
          <CardHead id="home-timeline-title" title={L("homeTimeline")} />
          {timeline.length === 0 ? (
            <EmptyState
              inline
              title={L("homeNoTimeline")}
              icon={<Icon.Clock />}
            />
          ) : (
            <Timeline items={timeline} label={L("homeTimeline")} />
          )}
        </Card>
      </div>
    </>
  );
}
