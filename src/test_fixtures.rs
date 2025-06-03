use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct TestMailDir {
    pub temp_dir: TempDir,
    pub root_path: PathBuf,
}

impl TestMailDir {
    pub fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let root_path = temp_dir.path().to_path_buf();

        let fixture = Self {
            temp_dir,
            root_path,
        };
        fixture.create_structure();
        fixture
    }

    fn create_structure(&self) {
        // Create INBOX folder with maildir structure
        self.create_maildir_folder("INBOX");

        // Create Sent folder
        self.create_maildir_folder("Sent");

        // Create Drafts folder
        self.create_maildir_folder("Drafts");

        // Create Trash folder
        self.create_maildir_folder("Trash");

        // Create Work folder with subfolders
        self.create_maildir_folder("Work");
        self.create_maildir_folder("Work/Projects");
        self.create_maildir_folder("Work/Meetings");

        // Create Personal folder with subfolders
        self.create_maildir_folder("Personal");
        self.create_maildir_folder("Personal/Family");
        self.create_maildir_folder("Personal/Friends");

        // Create Archive folder
        self.create_maildir_folder("Archive");
        self.create_maildir_folder("Archive/2023");
        self.create_maildir_folder("Archive/2024");

        // Add sample emails to various folders
        self.add_sample_emails();
    }

    fn create_maildir_folder(&self, folder_name: &str) {
        let folder_path = self.root_path.join(folder_name);
        fs::create_dir_all(&folder_path).expect("Failed to create folder");

        // Create maildir subdirectories
        fs::create_dir_all(folder_path.join("cur")).expect("Failed to create cur directory");
        fs::create_dir_all(folder_path.join("new")).expect("Failed to create new directory");
        fs::create_dir_all(folder_path.join("tmp")).expect("Failed to create tmp directory");
    }

    fn add_sample_emails(&self) {
        // Add emails to INBOX
        self.write_email(
            "INBOX/cur",
            "1234567890.email1",
            &self.create_welcome_email(),
        );
        self.write_email(
            "INBOX/cur",
            "1234567891.email2",
            &self.create_meeting_email(),
        );
        self.write_email(
            "INBOX/new",
            "1234567892.email3",
            &self.create_urgent_email(),
        );
        self.write_email(
            "INBOX/cur",
            "1234567893.email4",
            &self.create_newsletter_email(),
        );
        self.write_email(
            "INBOX/cur",
            "1234567894.email5",
            &self.create_attachment_email(),
        );

        // Add emails to Sent
        self.write_email("Sent/cur", "1234567895.sent1", &self.create_sent_response());
        self.write_email("Sent/cur", "1234567896.sent2", &self.create_sent_proposal());

        // Add emails to Work folders
        self.write_email("Work/cur", "1234567897.work1", &self.create_work_email());
        self.write_email(
            "Work/Projects/cur",
            "1234567898.project1",
            &self.create_project_email(),
        );
        self.write_email(
            "Work/Meetings/cur",
            "1234567899.meeting1",
            &self.create_meeting_reminder(),
        );

        // Add emails to Personal folders
        self.write_email(
            "Personal/cur",
            "1234567900.personal1",
            &self.create_personal_email(),
        );
        self.write_email(
            "Personal/Family/cur",
            "1234567901.family1",
            &self.create_family_email(),
        );
        self.write_email(
            "Personal/Friends/new",
            "1234567902.friend1",
            &self.create_friend_email(),
        );

        // Add archived emails
        self.write_email(
            "Archive/2023/cur",
            "1234567903.archive1",
            &self.create_old_email(),
        );
        self.write_email(
            "Archive/2024/cur",
            "1234567904.archive2",
            &self.create_recent_archive(),
        );

        // Add draft
        self.write_email(
            "Drafts/cur",
            "1234567905.draft1",
            &self.create_draft_email(),
        );

        // Add deleted email
        self.write_email(
            "Trash/cur",
            "1234567906.trash1",
            &self.create_deleted_email(),
        );
    }

    fn write_email(&self, folder: &str, filename: &str, content: &str) {
        let file_path = self.root_path.join(folder).join(filename);
        fs::write(file_path, content).expect("Failed to write email file");
    }

    fn create_welcome_email(&self) -> String {
        r#"Return-Path: <welcome@vulthor.example.com>
Delivered-To: user@example.com
Received: from smtp.vulthor.example.com (smtp.vulthor.example.com [192.168.1.100])
	by mx.example.com (Postfix) with ESMTP id 12345
	for <user@example.com>; Mon, 01 Jan 2024 10:00:00 +0000 (UTC)
From: Vulthor Team <welcome@vulthor.example.com>
To: user@example.com
Subject: Welcome to Vulthor Email Client!
Date: Mon, 01 Jan 2024 10:00:00 +0000
Message-ID: <welcome-001@vulthor.example.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8
Content-Transfer-Encoding: 7bit

Welcome to Vulthor!

Thank you for choosing Vulthor as your email client. This powerful TUI application
provides a fast and efficient way to manage your emails from the terminal.

Key features:
- Lightning-fast email browsing
- Vim-style navigation
- HTML email viewing via web interface
- Attachment support
- Hierarchical folder structure

Getting started:
1. Use j/k to navigate up and down
2. Press Tab to switch between panes
3. Use Enter to select folders or emails
4. Press ? for help at any time

We hope you enjoy using Vulthor!

Best regards,
The Vulthor Team

--
Vulthor Email Client
https://github.com/vulthor/vulthor"#
            .to_string()
    }

    fn create_meeting_email(&self) -> String {
        r#"From: Sarah Johnson <sarah.johnson@company.com>
To: user@example.com, team@company.com
Subject: Team Meeting Tomorrow - Project Sync
Date: Tue, 02 Jan 2024 14:30:00 +0000
Message-ID: <meeting-001@company.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8

Hi team,

Quick reminder about our team meeting tomorrow at 2 PM in Conference Room B.

Agenda:
- Q4 project review
- Q1 planning
- New hire introductions
- Budget discussions

Please come prepared with your project status updates.

Thanks,
Sarah

--
Sarah Johnson
Project Manager
Company Inc.
sarah.johnson@company.com"#
            .to_string()
    }

    fn create_urgent_email(&self) -> String {
        r#"From: Security Team <security@company.com>
To: user@example.com
Subject: [URGENT] Security Update Required
Date: Wed, 03 Jan 2024 09:15:00 +0000
Message-ID: <security-001@company.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8
X-Priority: 1

URGENT: Security Update Required

This is an urgent notification regarding a critical security update that must be
applied to all company systems by end of day today.

Action Required:
1. Update your system immediately
2. Restart all applications
3. Confirm completion by replying to this email

Failure to complete this update may result in system access restrictions.

If you have any questions or encounter issues, please contact the IT helpdesk
immediately at extension 1234.

Security Team
IT Department"#
            .to_string()
    }

    fn create_newsletter_email(&self) -> String {
        format!(
            r#"From: Tech News Daily <newsletter@technews.com>
To: user@example.com
Subject: Daily Tech Digest - AI Breakthrough & Rust 1.75 Released
Date: Thu, 04 Jan 2024 06:00:00 +0000
Message-ID: <newsletter-001@technews.com>
MIME-Version: 1.0
Content-Type: text/html; charset=UTF-8

<!DOCTYPE html>
<html>
<head>
    <title>Daily Tech Digest</title>
</head>
<body>
    <h1>Tech News Daily</h1>
    
    <h2>Today's Headlines</h2>
    
    <h3>🚀 AI Breakthrough in Natural Language Processing</h3>
    <p>Researchers at MIT announce a significant breakthrough in NLP that could revolutionize how we interact with computers...</p>
    
    <h3>🦀 Rust 1.75 Released with New Features</h3>
    <p>The latest version of Rust brings improved async support, better error messages, and enhanced performance optimizations...</p>
    
    <h3>📱 Mobile Development Trends for 2024</h3>
    <p>Industry experts predict the top mobile development trends that will shape the industry this year...</p>
    
    <hr>
    <p><small>You're receiving this because you subscribed to Tech News Daily.<br>
    <a href="{}unsubscribe">Unsubscribe</a> | <a href="{}preferences">Preferences</a></small></p>
</body>
</html>"#,
            "#", "#"
        )
    }

    fn create_attachment_email(&self) -> String {
        r#"From: John Doe <john.doe@example.com>
To: user@example.com
Subject: Project Documents and Photos
Date: Fri, 05 Jan 2024 16:45:00 +0000
Message-ID: <attachments-001@example.com>
MIME-Version: 1.0
Content-Type: multipart/mixed; boundary="boundary123"

--boundary123
Content-Type: text/plain; charset=UTF-8

Hi,

Please find the attached project documents and photos from our last meeting.

The PDF contains the detailed project specifications, and the images show
the current prototype status.

Let me know if you need any clarification on the documents.

Best,
John

--boundary123
Content-Type: application/pdf; name="project_specs.pdf"
Content-Disposition: attachment; filename="project_specs.pdf"
Content-Transfer-Encoding: base64

JVBERi0xLjQKJcOkw7zDtsOfCjIgMCBvYmoKPDwKL0xlbmd0aCAzIDAgUgo+PgpzdHJlYW0KQNOB
WjAiw4vDo8Ozw7PDt8OTw7fDs8Ozw7PDt8O3w7fDs8O3w7fDt8O3w7fDs8O3w7fDt8O3w7fDs8O3
w7fDt8O3w7fDs8O3w7fDt8O3w7fDs8O3w7fDt8O3w7fDs8O3w7fDt8O3w7fDs8O3w7fDt8O3w7fD
ZW5kc3RyZWFtCmVuZG9iagp4cmVmCjAgNAowMDAwMDAwMDAwIDY1NTM1IGYgCjAwMDAwMDAwMTAg
MDAwMDAgbiAKMDAwMDAwMDA3OSAwMDAwMCBuIAowMDAwMDAwMTczIDAwMDAwIG4gCnRyYWlsZXIK
PDwKL1NpemUgNAovUm9vdCAxIDAgUgo+PgpzdGFydHhyZWYKMjkzCiUlRU9GCg==

--boundary123
Content-Type: image/jpeg; name="prototype_photo.jpg"
Content-Disposition: attachment; filename="prototype_photo.jpg"
Content-Transfer-Encoding: base64

/9j/4AAQSkZJRgABAQEAYABgAAD/2wBDAAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB
AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQH/2wBDAQEBAQEBAQEBAQEBAQEBAQEBAQEB
AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQH/wAARCAABAAEDASIA
AhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAv/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFQEB
AQAAAAAAAAAAAAAAAAAAAAX/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIRAxEAPwDX4/8A
AH/Z

--boundary123--"#
            .to_string()
    }

    fn create_sent_response(&self) -> String {
        r#"From: user@example.com
To: sarah.johnson@company.com
Subject: Re: Team Meeting Tomorrow - Project Sync
Date: Tue, 02 Jan 2024 15:00:00 +0000
Message-ID: <response-001@example.com>
In-Reply-To: <meeting-001@company.com>
References: <meeting-001@company.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8

Hi Sarah,

Thanks for the reminder. I'll be there with my project status update.

Quick question - should I prepare anything specific for the budget discussion?

See you tomorrow,
User

On Tue, 02 Jan 2024 at 14:30, Sarah Johnson wrote:
> Hi team,
>
> Quick reminder about our team meeting tomorrow at 2 PM in Conference Room B.
> [...]"#
            .to_string()
    }

    fn create_sent_proposal(&self) -> String {
        r#"From: user@example.com
To: client@external.com
Subject: Proposal for Web Development Project
Date: Wed, 03 Jan 2024 11:20:00 +0000
Message-ID: <proposal-001@example.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8

Dear Client,

Thank you for your interest in our web development services. I'm pleased to
submit our proposal for your new e-commerce website project.

Project Overview:
- Modern responsive design
- Shopping cart functionality
- Payment gateway integration
- Admin dashboard
- SEO optimization

Timeline: 8-10 weeks
Budget: $15,000 - $20,000

I'd be happy to discuss this proposal in detail. Please let me know when
you'd be available for a call.

Best regards,
User Name
Senior Developer"#
            .to_string()
    }

    fn create_work_email(&self) -> String {
        r#"From: manager@company.com
To: user@example.com
Subject: Q1 Performance Review Schedule
Date: Thu, 04 Jan 2024 13:15:00 +0000
Message-ID: <work-001@company.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8

Hi,

Your Q1 performance review has been scheduled for January 15th at 3 PM.

Please prepare:
- Self-assessment form (attached in separate email)
- Goal achievements from Q4
- Q1 objectives

The review will take place in my office. Let me know if you need to reschedule.

Thanks,
Manager"#
            .to_string()
    }

    fn create_project_email(&self) -> String {
        r#"From: lead.developer@company.com
To: user@example.com, dev-team@company.com
Subject: Project Alpha - Code Review Required
Date: Fri, 05 Jan 2024 10:30:00 +0000
Message-ID: <project-001@company.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8

Team,

The initial implementation for Project Alpha is ready for review.

GitHub PR: https://github.com/company/project-alpha/pull/42

Key changes:
- Authentication module refactoring
- Database migration scripts
- Updated API endpoints
- Test coverage improvements

Please review by Monday. Priority is medium.

Best,
Lead Developer"#
            .to_string()
    }

    fn create_meeting_reminder(&self) -> String {
        r#"From: calendar@company.com
To: user@example.com
Subject: Reminder: Sprint Planning Meeting in 15 minutes
Date: Mon, 08 Jan 2024 08:45:00 +0000
Message-ID: <reminder-001@company.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8
X-Auto-Response-Suppress: All

This is an automatic reminder for your upcoming meeting:

Sprint Planning Meeting
Time: 9:00 AM - 10:00 AM
Location: Conference Room A
Attendees: Development Team

Agenda:
- Review previous sprint
- Plan upcoming sprint tasks
- Estimate story points
- Assign responsibilities

Meeting link: https://meet.company.com/sprint-planning

--
Company Calendar System"#
            .to_string()
    }

    fn create_personal_email(&self) -> String {
        r#"From: bank@mybank.com
To: user@example.com
Subject: Monthly Account Statement Available
Date: Sat, 06 Jan 2024 12:00:00 +0000
Message-ID: <statement-001@mybank.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8

Dear Customer,

Your monthly account statement for December 2023 is now available.

Account Summary:
- Starting Balance: $2,450.00
- Total Deposits: $3,200.00
- Total Withdrawals: $1,875.50
- Ending Balance: $3,774.50

You can view your complete statement by logging into online banking
or visiting any of our branch locations.

Thank you for choosing MyBank.

Sincerely,
MyBank Customer Service"#
            .to_string()
    }

    fn create_family_email(&self) -> String {
        r#"From: mom@family.com
To: user@example.com
Subject: Family Reunion Plans
Date: Sun, 07 Jan 2024 19:30:00 +0000
Message-ID: <family-001@family.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8

Hi sweetie,

Hope you're doing well! I wanted to update you on the family reunion plans.

We've decided on July 15-17 at Grandma's house. Uncle Bob is organizing
the BBQ, and Aunt Mary is handling accommodations for out-of-town family.

Can you let me know if those dates work for you? Also, do you have any
dietary restrictions we should know about?

Looking forward to seeing everyone!

Love,
Mom

P.S. - Don't forget to call Grandma for her birthday next week!"#
            .to_string()
    }

    fn create_friend_email(&self) -> String {
        r#"From: bestfriend@email.com
To: user@example.com
Subject: Movie Night This Weekend?
Date: Fri, 05 Jan 2024 20:15:00 +0000
Message-ID: <friend-001@email.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8

Hey!

Want to do movie night this weekend? I heard the new sci-fi movie is pretty good,
or we could binge-watch that series we started last month.

I can bring the popcorn if you've got the drinks! 🍿

Let me know what works for you. Saturday or Sunday evening both work for me.

Talk soon!
Best Friend

P.S. - Thanks again for helping me move last weekend. You're awesome! 😊"#
            .to_string()
    }

    fn create_old_email(&self) -> String {
        r#"From: newsletter@oldcompany.com
To: user@example.com
Subject: Company Newsletter - March 2023
Date: Wed, 15 Mar 2023 10:00:00 +0000
Message-ID: <newsletter-2023-03@oldcompany.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8

COMPANY NEWSLETTER - MARCH 2023

Welcome to our monthly newsletter! Here's what's happening:

NEW HIRES:
- Welcome to Jane Smith, our new Marketing Director
- John Doe joins the Development team

UPCOMING EVENTS:
- Company picnic: April 20th
- Quarterly meeting: March 30th

ACHIEVEMENTS:
- Reached 1 million users milestone
- Won "Best Workplace" award

Stay tuned for more updates!

HR Team"#
            .to_string()
    }

    fn create_recent_archive(&self) -> String {
        r#"From: conferences@techconf.com
To: user@example.com
Subject: Thank you for attending TechConf 2024!
Date: Fri, 15 Nov 2024 16:00:00 +0000
Message-ID: <thankyou-001@techconf.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8

Dear Attendee,

Thank you for making TechConf 2024 a huge success!

Conference Highlights:
- 500+ attendees
- 30 amazing speakers
- 15 workshop sessions
- Countless networking opportunities

Resources:
- Presentation slides: https://techconf.com/2024/slides
- Video recordings: Available next week
- Speaker contact info: In the conference app

We hope to see you again next year!

Best regards,
TechConf Organizing Committee"#
            .to_string()
    }

    fn create_draft_email(&self) -> String {
        r#"From: user@example.com
To: 
Subject: Draft: Follow-up on Project Discussion
Date: Mon, 08 Jan 2024 09:30:00 +0000
Message-ID: <draft-001@example.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8
X-Draft: true

Hi [Name],

Following up on our discussion yesterday about the new project proposal.

I've been thinking about the timeline and budget considerations we discussed:

- Phase 1: Research and planning (2 weeks)
- Phase 2: Initial development (6 weeks)  
- Phase 3: Testing and refinement (2 weeks)

Budget estimate: $[AMOUNT]

Let me know your thoughts on this approach.

Best,
[Need to finish this...]"#
            .to_string()
    }

    fn create_deleted_email(&self) -> String {
        r#"From: spam@suspicious.com
To: user@example.com
Subject: You've Won $1,000,000!!! Click Here NOW!!!
Date: Tue, 02 Jan 2024 03:22:00 +0000
Message-ID: <spam-001@suspicious.com>
MIME-Version: 1.0
Content-Type: text/html; charset=UTF-8

<!DOCTYPE html>
<html>
<body>
<h1 style="color: red; font-size: 24px;">CONGRATULATIONS!!!</h1>

<p>You have been selected as our GRAND PRIZE WINNER!</p>

<p>You've won $1,000,000 in our exclusive lottery!</p>

<p style="color: red; font-weight: bold;">
CLICK HERE IMMEDIATELY to claim your prize:
<a href="http://suspicious.com/claim">CLAIM NOW!!!</a>
</p>

<p><small>This offer expires in 24 hours! Act fast!</small></p>

</body>
</html>"#
            .to_string()
    }

    pub fn get_folder_path(&self, folder: &str) -> PathBuf {
        self.root_path.join(folder)
    }

    pub fn get_email_count(&self, folder: &str) -> usize {
        let cur_path = self.get_folder_path(folder).join("cur");
        let new_path = self.get_folder_path(folder).join("new");

        let mut count = 0;

        if cur_path.exists() {
            count += fs::read_dir(cur_path).unwrap().count();
        }

        if new_path.exists() {
            count += fs::read_dir(new_path).unwrap().count();
        }

        count
    }

    pub fn create_empty_folder(&self, folder_name: &str) {
        self.create_maildir_folder(folder_name);
    }

    pub fn add_custom_email(&self, folder: &str, filename: &str, content: &str) {
        self.write_email(&format!("{}/cur", folder), filename, content);
    }

    pub fn add_unread_email(&self, folder: &str, filename: &str, content: &str) {
        self.write_email(&format!("{}/new", folder), filename, content);
    }

    pub fn list_folders(&self) -> Vec<String> {
        let mut folders = Vec::new();
        self.collect_folders(&self.root_path, "", &mut folders);
        folders.sort();
        folders
    }

    fn collect_folders(&self, path: &Path, prefix: &str, folders: &mut Vec<String>) {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    // Skip maildir special directories
                    if name == "cur" || name == "new" || name == "tmp" {
                        continue;
                    }

                    let folder_name = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{}", prefix, name)
                    };

                    folders.push(folder_name.clone());
                    self.collect_folders(&entry.path(), &folder_name, folders);
                }
            }
        }
    }

    /// Get statistics about the test maildir
    pub fn get_stats(&self) -> TestMaildirStats {
        let folders = self.list_folders();
        let mut total_emails = 0;
        let mut unread_emails = 0;

        for folder in &folders {
            let cur_count = self.count_emails_in_subdir(&format!("{}/cur", folder));
            let new_count = self.count_emails_in_subdir(&format!("{}/new", folder));
            total_emails += cur_count + new_count;
            unread_emails += new_count;
        }

        TestMaildirStats {
            total_folders: folders.len(),
            total_emails,
            unread_emails,
            folders,
        }
    }

    fn count_emails_in_subdir(&self, subdir: &str) -> usize {
        let path = self.root_path.join(subdir);
        if path.exists() && path.is_dir() {
            fs::read_dir(path)
                .map(|entries| entries.count())
                .unwrap_or(0)
        } else {
            0
        }
    }
}

#[derive(Debug, Clone)]
pub struct TestMaildirStats {
    pub total_folders: usize,
    pub total_emails: usize,
    pub unread_emails: usize,
    pub folders: Vec<String>,
}

impl TestMaildirStats {
    pub fn print_summary(&self) {
        println!("Test MailDir Statistics:");
        println!("  Folders: {}", self.total_folders);
        println!("  Total Emails: {}", self.total_emails);
        println!("  Unread Emails: {}", self.unread_emails);
        println!("  Folder List: {:?}", self.folders);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_maildir_creation() {
        let test_maildir = TestMailDir::new();
        let stats = test_maildir.get_stats();

        // Verify we have the expected folder structure
        assert!(stats.total_folders >= 10); // Should have at least the main folders
        assert!(stats.total_emails > 15); // Should have at least 15+ test emails
        assert!(stats.unread_emails >= 2); // Should have some unread emails

        // Verify specific folders exist
        assert!(stats.folders.contains(&"INBOX".to_string()));
        assert!(stats.folders.contains(&"Sent".to_string()));
        assert!(stats.folders.contains(&"Work".to_string()));
        assert!(stats.folders.contains(&"Work/Projects".to_string()));
        assert!(stats.folders.contains(&"Personal/Family".to_string()));
    }

    #[test]
    fn test_inbox_has_emails() {
        let test_maildir = TestMailDir::new();
        let inbox_count = test_maildir.get_email_count("INBOX");
        assert!(inbox_count >= 5); // INBOX should have at least 5 test emails
    }

    #[test]
    fn test_folder_structure() {
        let test_maildir = TestMailDir::new();

        // Verify maildir structure exists for INBOX
        let inbox_path = test_maildir.get_folder_path("INBOX");
        assert!(inbox_path.join("cur").exists());
        assert!(inbox_path.join("new").exists());
        assert!(inbox_path.join("tmp").exists());

        // Verify Work subfolder structure
        let work_projects_path = test_maildir.get_folder_path("Work/Projects");
        assert!(work_projects_path.exists());
        assert!(work_projects_path.join("cur").exists());
    }

    #[test]
    fn test_custom_email_addition() {
        let test_maildir = TestMailDir::new();

        // Add a custom email
        let custom_email = r#"From: test@example.com
To: user@example.com
Subject: Test Email
Date: Mon, 01 Jan 2024 12:00:00 +0000
Message-ID: <test@example.com>

This is a test email."#;

        test_maildir.add_custom_email("INBOX", "test_email", custom_email);

        // Verify it was added
        let new_count = test_maildir.get_email_count("INBOX");
        assert!(new_count >= 6); // Should now have one more email
    }

    #[test]
    fn test_unread_email_addition() {
        let test_maildir = TestMailDir::new();

        // Add an unread email
        let unread_email = r#"From: urgent@example.com
To: user@example.com
Subject: Urgent: Please Read
Date: Mon, 01 Jan 2024 12:00:00 +0000
Message-ID: <urgent@example.com>

This is an urgent email."#;

        test_maildir.add_unread_email("INBOX", "urgent_email", unread_email);

        // Verify the email was added to the 'new' folder
        let new_path = test_maildir.get_folder_path("INBOX").join("new");
        let unread_count = fs::read_dir(new_path).unwrap().count();
        assert!(unread_count >= 2); // Should have at least 2 unread emails now
    }
}
