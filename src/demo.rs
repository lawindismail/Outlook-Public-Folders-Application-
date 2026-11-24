//! Offline demo folders, so the app can be tried without a server.

use crate::model::*;
use chrono::{Duration, Local};
use std::collections::HashMap;

pub struct Demo {
    children: HashMap<String, Vec<FolderInfo>>,
    items: HashMap<String, Vec<ItemDetail>>,
    dates: HashMap<String, chrono::DateTime<Local>>,
}

struct Seed {
    from: &'static str,
    email: &'static str,
    subject: &'static str,
    body: &'static str,
    hours_ago: i64,
    attachment: Option<&'static str>,
}

fn s(from: &'static str, email: &'static str, subject: &'static str, body: &'static str, hours_ago: i64, attachment: Option<&'static str>) -> Seed {
    Seed { from, email, subject, body, hours_ago, attachment }
}

impl Demo {
    pub fn new() -> Self {
        let mut d = Demo { children: HashMap::new(), items: HashMap::new(), dates: HashMap::new() };
        let f = |id: &str, name: &str, class: &str| FolderInfo {
            id: id.into(),
            name: name.into(),
            class: class.into(),
            child_count: 0,
            total_count: 0,
        };
        let root = vec![
            f("ann", "Company Announcements", "IPF.Note"),
            f("hr", "HR Policies", "IPF.Post"),
            f("it", "IT Help Desk", "IPF.Note"),
            f("proj", "Projects", "IPF.Note"),
            f("sales", "Sales Team", "IPF.Note"),
            f("cal", "Shared Calendars", "IPF.Note"),
        ];
        d.children.insert(String::new(), root);
        d.children.insert("it".into(), vec![f("it-arch", "Archive 2025", "IPF.Note"), f("it-howto", "How-to Guides", "IPF.Post")]);
        d.children.insert("proj".into(), vec![f("proj-move", "Office Move", "IPF.Note"), f("proj-web", "Website Refresh", "IPF.Note")]);
        d.children.insert("sales".into(), vec![f("sales-contacts", "Customer Contacts", "IPF.Contact"), f("sales-leads", "Leads", "IPF.Post")]);
        d.children.insert("cal".into(), vec![f("cal-hol", "Holidays", "IPF.Appointment"), f("cal-rooms", "Meeting Rooms", "IPF.Appointment")]);

        d.add("ann", "IPM.Note", vec![
            s("Office Manager", "office@example.com", "Building closed on Friday for maintenance", "Hello everyone,\n\nThe building will be closed this Friday while the heating system is serviced. Please work from home that day. Deliveries will be held at reception until Monday.\n\nThanks for your patience,\nOffice Manager", 2, None),
            s("Leadership Team", "leaders@example.com", "Quarterly update and town hall", "Hi all,\n\nOur quarterly town hall is next Thursday at 10:00 in the main hall and online. We'll cover results, the roadmap for next quarter and answer your questions.\n\nThe slides are attached.\n\nSee you there.", 26, Some("Town hall slides.pptx")),
            s("IT Services", "it@example.com", "New Outlook rollout — what changes for you", "Hello,\n\nWe're moving to the new Outlook over the coming weeks. Mail, calendar and contacts work as before. Public folders aren't shown in the new Outlook, so use the public folder viewer to keep reading them.\n\nIT Services", 50, None),
            s("Facilities", "facilities@example.com", "Bike storage now open", "The new bike storage next to the car park is open. Ask reception for a key card.", 96, None),
            s("Social Club", "social@example.com", "End of year party — save the date", "Save the date: our end of year party is on 12 December. More details soon!", 170, None),
            s("Office Manager", "office@example.com", "Fire drill results", "Thanks for taking part in the fire drill. Everyone was out in under four minutes. Well done.", 340, None),
        ]);
        d.add("hr", "IPM.Post", vec![
            s("HR Team", "hr@example.com", "Holiday request policy (updated)", "Please request leave at least two weeks ahead through the HR portal. Requests over the holiday period are approved in the order they arrive.", 30, Some("Holiday policy.pdf")),
            s("HR Team", "hr@example.com", "Working from home guidelines", "You can work from home up to three days a week. Agree your pattern with your manager and keep your calendar up to date.", 400, None),
            s("HR Team", "hr@example.com", "Expenses: how to claim", "Submit receipts within 30 days. Travel is reimbursed at the standard rate.", 900, Some("Expense form.xlsx")),
        ]);
        d.add("it", "IPM.Note", vec![
            s("Sam Patel", "sam.patel@example.com", "Printer on 2nd floor jams", "The printer by the kitchen on the second floor keeps jamming on double-sided jobs. Could someone take a look?", 5, None),
            s("IT Services", "it@example.com", "RE: VPN disconnects every hour", "We've pushed a fix for the VPN client. Restart your laptop to pick it up and let us know if it happens again.", 28, None),
            s("Alex Morgan", "alex.morgan@example.com", "Request: second monitor", "Hi, could I get a second monitor for my desk? Thanks!", 75, None),
        ]);
        d.add("it-arch", "IPM.Note", vec![s("IT Services", "it@example.com", "Password reset portal moved", "The password reset portal has a new address. The old one redirects for now.", 2400, None)]);
        d.add("it-howto", "IPM.Post", vec![
            s("IT Services", "it@example.com", "How to connect to the office Wi-Fi", "Choose the network named Office, sign in with your work account and accept the certificate.", 600, None),
            s("IT Services", "it@example.com", "How to share a calendar", "Open the calendar, choose Share and add the people you want. Pick how much they can see.", 800, None),
        ]);
        d.add("proj-move", "IPM.Note", vec![
            s("Jordan Lee", "jordan.lee@example.com", "Floor plan v3", "Here's the latest floor plan. Teams on the left wing move first.", 40, Some("Floor plan v3.pdf")),
            s("Jordan Lee", "jordan.lee@example.com", "Packing crates arrive Monday", "Crates will be delivered Monday morning. Label each one with your name and desk number.", 130, None),
        ]);
        d.add("proj-web", "IPM.Note", vec![s("Riley Chen", "riley.chen@example.com", "Homepage mock-ups for review", "Three homepage options are attached. Please send feedback by Wednesday.", 20, Some("Homepage options.png"))]);
        d.add("sales-leads", "IPM.Post", vec![
            s("Casey Brooks", "casey.brooks@example.com", "Lead: regional bakery chain", "Spoke with the operations lead. They want a demo next week for 14 shops.", 8, None),
            s("Morgan Diaz", "morgan.diaz@example.com", "Lead: city library network", "They're tendering in January. I'll prepare the response pack.", 60, None),
        ]);
        d.add("sales", "IPM.Note", vec![s("Sales Manager", "sales@example.com", "Targets for next quarter", "Targets are in the attached sheet. Let's review them in Monday's meeting.", 12, Some("Targets.xlsx"))]);
        d.add("cal-hol", "IPM.Appointment", vec![
            s("HR Team", "hr@example.com", "Office closed — public holiday", "The office is closed for the public holiday.", -300, None),
            s("HR Team", "hr@example.com", "Office closes at 14:00", "Early finish before the holidays.", -200, None),
        ]);
        d.add("cal-rooms", "IPM.Appointment", vec![
            s("Jordan Lee", "jordan.lee@example.com", "Board room — project review", "Booked for the office move project review.", -20, None),
            s("Riley Chen", "riley.chen@example.com", "Room 2 — design workshop", "Website design workshop.", -44, None),
        ]);
        d.add_contacts();

        // Fill in folder counts.
        let counts: HashMap<String, (u32, u32)> = d
            .children
            .values()
            .flatten()
            .map(|f| {
                let items = d.items.get(&f.id).map(|v| v.len() as u32).unwrap_or(0);
                let subs = d.children.get(&f.id).map(|v| v.len() as u32).unwrap_or(0);
                (f.id.clone(), (items, subs))
            })
            .collect();
        for list in d.children.values_mut() {
            for f in list.iter_mut() {
                let (items, subs) = counts[&f.id];
                f.total_count = items;
                f.child_count = subs;
            }
        }
        d
    }

    fn add(&mut self, folder: &str, class: &str, seeds: Vec<Seed>) {
        let now = Local::now();
        let list = seeds
            .into_iter()
            .enumerate()
            .map(|(i, sd)| {
                let id = format!("{folder}:{i}");
                let when = now - Duration::hours(sd.hours_ago) - Duration::minutes((i as i64 * 17) % 50);
                self.dates.insert(id.clone(), when);
                let calendar = class == "IPM.Appointment";
                let mut fields = Vec::new();
                if calendar {
                    fields.push(("Starts".to_string(), fmt_long_date(&when)));
                    fields.push(("Ends".to_string(), fmt_long_date(&(when + Duration::hours(1)))));
                }
                ItemDetail {
                    id,
                    subject: sd.subject.into(),
                    from: sd.from.into(),
                    from_email: sd.email.into(),
                    to: if calendar { String::new() } else { format!("{} (public folder)", folder_title(folder)) },
                    cc: String::new(),
                    date: fmt_long_date(&when),
                    body: sd.body.into(),
                    class: class.into(),
                    fields,
                    attachments: sd
                        .attachment
                        .map(|n| vec![Attachment { id: format!("{folder}:{i}:a"), name: n.into(), size: 182_000 + i as u64 * 9_000 }])
                        .unwrap_or_default(),
                }
            })
            .collect();
        self.items.insert(folder.into(), list);
    }

    fn add_contacts(&mut self) {
        let people = [
            ("Avery Collins", "Northwind Bakeries", "Operations Lead", "avery.collins@example.org", "+44 20 7946 0101"),
            ("Blake Turner", "Harbour Libraries", "Procurement Officer", "blake.turner@example.org", "+44 20 7946 0102"),
            ("Drew Fischer", "Lakeside Clinics", "IT Manager", "drew.fischer@example.org", "+44 20 7946 0103"),
            ("Emery Walsh", "Summit Outdoor", "Buyer", "emery.walsh@example.org", "+44 20 7946 0104"),
        ];
        let list = people
            .iter()
            .enumerate()
            .map(|(i, (name, company, title, email, phone))| ItemDetail {
                id: format!("sales-contacts:{i}"),
                subject: name.to_string(),
                from: name.to_string(),
                from_email: email.to_string(),
                body: format!("Met at the trade fair. Interested in a follow-up call about {company}'s plans."),
                class: "IPM.Contact".into(),
                fields: vec![
                    ("Company".into(), company.to_string()),
                    ("Job title".into(), title.to_string()),
                    ("Email".into(), email.to_string()),
                    ("Phone (Business)".into(), phone.to_string()),
                ],
                ..Default::default()
            })
            .collect();
        self.items.insert("sales-contacts".into(), list);
    }

    fn find_item(&self, folder_id: &str, item_id: &str) -> Option<&ItemDetail> {
        self.items.get(folder_id)?.iter().find(|i| i.id == item_id)
    }
}

fn folder_title(id: &str) -> &'static str {
    match id {
        "ann" => "Company Announcements",
        "hr" => "HR Policies",
        "it" | "it-arch" | "it-howto" => "IT Help Desk",
        "proj-move" => "Office Move",
        "proj-web" => "Website Refresh",
        "sales" | "sales-leads" => "Sales Team",
        _ => "Public Folders",
    }
}

impl Backend for Demo {
    fn label(&self) -> String {
        "Demo folders (no server)".into()
    }
    fn root_folders(&self) -> Result<Vec<FolderInfo>, String> {
        Ok(self.children.get("").cloned().unwrap_or_default())
    }
    fn child_folders(&self, folder_id: &str) -> Result<Vec<FolderInfo>, String> {
        Ok(self.children.get(folder_id).cloned().unwrap_or_default())
    }
    fn items(&self, folder: &FolderInfo, _offset: u32) -> Result<ItemPage, String> {
        let list = self.items.get(&folder.id).cloned().unwrap_or_default();
        let items: Vec<ItemSummary> = list
            .iter()
            .map(|d| {
                let calendar = d.class == "IPM.Appointment";
                let contact = d.class == "IPM.Contact";
                let when = self.dates.get(&d.id);
                ItemSummary {
                    id: d.id.clone(),
                    subject: if contact { format!("{} · {}", d.fields[0].1, d.fields[1].1) } else { d.subject.clone() },
                    from: d.from.clone(),
                    date: when.map(fmt_list_date).unwrap_or_default(),
                    class: d.class.clone(),
                    has_attachments: !d.attachments.is_empty(),
                    preview: if calendar {
                        d.fields.first().map(|f| f.1.clone()).unwrap_or_default()
                    } else if contact {
                        d.fields[2].1.clone()
                    } else {
                        d.body.replace('\n', " ")
                    },
                    when: when.copied(),
                }
            })
            .collect();
        Ok(ItemPage { total: items.len() as u32, more: false, next_offset: items.len() as u32, items })
    }
    fn item(&self, folder_id: &str, item_id: &str) -> Result<ItemDetail, String> {
        self.find_item(folder_id, item_id).cloned().ok_or_else(|| "Item not found.".into())
    }
    fn attachment(&self, _folder_id: &str, attachment_id: &str) -> Result<Vec<u8>, String> {
        Ok(format!("Demo attachment {attachment_id}. In demo mode attachments are placeholders.\n").into_bytes())
    }
    fn mime(&self, folder_id: &str, item_id: &str) -> Result<Vec<u8>, String> {
        let d = self.find_item(folder_id, item_id).ok_or("Item not found.")?;
        Ok(format!(
            "From: {} <{}>\r\nSubject: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n",
            d.from,
            d.from_email,
            d.subject,
            d.body.replace('\n', "\r\n")
        )
        .into_bytes())
    }
}
