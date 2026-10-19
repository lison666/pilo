import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import type { PiloClient } from "@/lib/api/pilo-client";
import {
	addProject,
	notifyProjectsChanged,
	type Project,
} from "@/lib/api/projects";
import { userErrorMessage } from "@/lib/app-error";
import {
	Button,
	Dialog,
	DialogContent,
	DialogDescription,
	DialogHeader,
	DialogTitle,
	Input,
	Label,
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
	Textarea,
} from "@/ui";

type Purpose = "job" | "troubleshoot" | "analysis" | "api-analysis";

type FieldSpec = {
	key: string;
	required?: boolean;
	multiline?: boolean;
};

const PURPOSE_FIELDS: Record<Purpose, FieldSpec[]> = {
	job: [
		{ key: "interface_name", required: true },
		{ key: "entry_url" },
		{ key: "job_id" },
		{ key: "task_name" },
		{ key: "spider_name" },
		{ key: "script_file" },
		{ key: "objective", multiline: true },
	],
	troubleshoot: [
		{ key: "slug", required: true },
		{ key: "question", required: true, multiline: true },
		{ key: "job_ref" },
	],
	analysis: [
		{ key: "slug", required: true },
		{ key: "question", required: true, multiline: true },
		{ key: "job_ref" },
	],
	"api-analysis": [
		{ key: "slug", required: true },
		{ key: "api_url", required: true },
		{ key: "question", required: true, multiline: true },
		{ key: "evidence_goal" },
		{ key: "job_ref" },
	],
};

/** Registry-backed job creation only needs interface_name (+ optional entry_url). */
const REGISTRY_KEYS = new Set(["interface_name", "entry_url"]);

export function BiddingNewDialog({
	open,
	onOpenChange,
	project,
	client,
	onCreated,
}: {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	project: Project;
	client: PiloClient;
	onCreated: (created: Project) => void;
}) {
	const { t } = useTranslation();
	const [purpose, setPurpose] = useState<Purpose>("job");
	const [fromRegistry, setFromRegistry] = useState(true);
	const [values, setValues] = useState<Record<string, string>>({});
	const [submitting, setSubmitting] = useState(false);

	const fields = useMemo(() => {
		const specs = PURPOSE_FIELDS[purpose];
		if (purpose === "job" && fromRegistry)
			return specs.filter((s) => REGISTRY_KEYS.has(s.key));
		return specs;
	}, [purpose, fromRegistry]);

	const submit = async () => {
		const payload: Record<string, string> = {};
		for (const spec of fields) {
			const value = (values[spec.key] ?? "").trim();
			if (spec.required && !value) {
				toast.error(t("bidding.missingField", { field: spec.key }));
				return;
			}
			if (value) payload[spec.key] = value;
		}
		setSubmitting(true);
		try {
			const result = await client.biddingAgentNew({
				projectId: project.id,
				purpose,
				fields: payload,
				fromRegistry: purpose === "job" && fromRegistry,
			});
			if (result.code !== 0) {
				toast.error(result.stderr.trim() || t("bidding.failed"));
				return;
			}
			const path = result.stdout.trim().split(/\r?\n/).pop() ?? "";
			const created = await addProject(project.connection.id, path);
			notifyProjectsChanged();
			toast.success(t("bidding.created", { path }));
			onOpenChange(false);
			setValues({});
			onCreated(created);
		} catch (error) {
			toast.error(userErrorMessage(error));
		} finally {
			setSubmitting(false);
		}
	};

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-md">
				<DialogHeader>
					<DialogTitle>{t("bidding.title")}</DialogTitle>
					<DialogDescription>{t("bidding.description")}</DialogDescription>
				</DialogHeader>
				<div className="flex flex-col gap-3">
					<div className="flex flex-col gap-1.5">
						<Label>{t("bidding.purpose")}</Label>
						<Select
							value={purpose}
							onValueChange={(v) => setPurpose(v as Purpose)}
						>
							<SelectTrigger>
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								{(Object.keys(PURPOSE_FIELDS) as Purpose[]).map((p) => (
									<SelectItem key={p} value={p}>
										{t(`bidding.purposes.${p}`)}
									</SelectItem>
								))}
							</SelectContent>
						</Select>
					</div>
					{purpose === "job" && (
						<label className="flex items-center gap-2 text-sm">
							<input
								type="checkbox"
								checked={fromRegistry}
								onChange={(e) => setFromRegistry(e.target.checked)}
							/>
							{t("bidding.fromRegistry")}
						</label>
					)}
					{fields.map((spec) => (
						<div key={spec.key} className="flex flex-col gap-1.5">
							<Label>
								{t(`bidding.fields.${spec.key}`, spec.key)}
								{spec.required ? " *" : ""}
							</Label>
							{spec.multiline ? (
								<Textarea
									value={values[spec.key] ?? ""}
									onChange={(e) =>
										setValues((v) => ({ ...v, [spec.key]: e.target.value }))
									}
									rows={3}
								/>
							) : (
								<Input
									value={values[spec.key] ?? ""}
									onChange={(e) =>
										setValues((v) => ({ ...v, [spec.key]: e.target.value }))
									}
								/>
							)}
						</div>
					))}
					<div className="flex justify-end gap-2 pt-1">
						<Button variant="ghost" onClick={() => onOpenChange(false)}>
							{t("bidding.cancel")}
						</Button>
						<Button disabled={submitting} onClick={() => void submit()}>
							{submitting ? t("bidding.creating") : t("bidding.create")}
						</Button>
					</div>
				</div>
			</DialogContent>
		</Dialog>
	);
}
