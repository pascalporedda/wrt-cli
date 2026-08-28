#compdef wrt

_wrt_worktrees() {
  local -a names
  names=(${(f)"$(command wrt ls 2>/dev/null | awk 'NF && $1 != "(no" {print $1}')"})
  _describe -t worktrees 'worktree' names
}

_wrt_branches() {
  local -a branches
  branches=(${(f)"$(command git for-each-ref --format='%(refname) %(symref)' refs/heads refs/remotes 2>/dev/null | awk 'NF == 1 { name = $1; sub("^refs/heads/", "", name); sub("^refs/remotes/[^/]+/", "", name); if (!seen[name]++) print name }')"})
  _describe -t branches 'branch' branches
}

_wrt_runtime_target() {
  _alternative \
    'actions:action:(start stop status)' \
    'worktrees:worktree:_wrt_worktrees'
}

_wrt_db_target() {
  _alternative \
    'actions:action:(reset seed migrate)' \
    'worktrees:worktree:_wrt_worktrees'
}

_wrt_db_action() {
  local index skip_value=0
  REPLY=

  for (( index = 3; index < CURRENT; index++ )); do
    if (( skip_value )); then
      skip_value=0
      continue
    fi

    case $words[index] in
      --worktree) skip_value=1 ;;
      --worktree=*) ;;
      reset|seed|migrate)
        REPLY=$words[index]
        return
        ;;
    esac
  done
}

_wrt() {
  local context state state_descr line

  _arguments -C \
    '1:command:->cmds' \
    '*::arg:->args'

  case $state in
    cmds)
      _values 'command' \
        'help[Print usage]' \
        'init[Generate shared managed-root config]' \
        'clone[Clone into a managed root]' \
        'root[Manage bare-root environments]' \
        'new[Create a new worktree]' \
        'add[Create a new worktree]' \
        'db[Run database utilities]' \
        'ls[List tracked worktrees]' \
        'path[Print worktree path]' \
        'env[Print exports for a worktree]' \
        'doctor[Check Compose worktree isolation]' \
        'setup[Retry setup for a worktree]' \
        'runtime[Run a repository-owned runtime command]' \
        'rm[Remove a worktree]' \
        'remove[Remove a worktree]' \
        'prune[Prune git worktrees and state]' \
        'housekeeping[Clean old unused branches]' \
        'run[Run a command in a worktree]' \
        'completions[Generate zsh completions]'
      return
      ;;
    args)
      case $line[1] in
        rm|remove)
          _arguments -C \
            '1:worktree:_wrt_worktrees' \
            '--force[Force remove]' \
            '--delete-branch[Delete branch]'
          return
          ;;
        path|setup)
          _arguments '1:worktree:_wrt_worktrees'
          return
          ;;
        env|doctor)
          _arguments '1::worktree:_wrt_worktrees'
          return
          ;;
        runtime)
          case $line[2] in
            start|stop|status)
              _arguments \
                '--worktree=[Explicit worktree name]:worktree:_wrt_worktrees'
              return
              ;;
          esac
          _arguments -C \
            '1:worktree or action:_wrt_runtime_target' \
            '2:action:(start stop status)' \
            '--worktree=[Explicit worktree name]:worktree:_wrt_worktrees'
          return
          ;;
        run)
          _arguments -C '1:worktree:_wrt_worktrees' '*::command:_command_names -e'
          return
          ;;
        db)
          local db_action
          _wrt_db_action
          db_action=$REPLY

          case $db_action in
            reset)
              _arguments -C \
                '--print[Print the command that would be run and exit]' \
                '--yes[Skip the reset confirmation]' \
                '--worktree=[Explicit worktree name]:worktree:_wrt_worktrees'
              ;;
            seed|migrate)
              _arguments -C \
                '--print[Print the command that would be run and exit]' \
                '--worktree=[Explicit worktree name]:worktree:_wrt_worktrees'
              ;;
            *)
              _arguments -C \
                '1:worktree or action:_wrt_db_target' \
                '2:action:(reset seed migrate)' \
                '--worktree=[Explicit worktree name]:worktree:_wrt_worktrees'
              ;;
          esac
          return
          ;;
        new|add)
          _arguments -C \
            '1:name:_wrt_branches' \
            '--from=[Start ref]:ref:' \
            '--branch=[Branch name]:branch:' \
            '--install=[Install deps]:mode:(auto true false)' \
            '--supabase=[Supabase]:mode:(auto shared isolated none true false)' \
            '--supabase-config=[Repo-relative Supabase config path]:file:_files' \
            '--db=[DB setup]:mode:(auto true false)' \
            '--cd[Print cd snippet]'
          return
          ;;
        clone)
          _arguments -C \
            '1:git repository:_files' \
            '--root=[Managed root directory]:directory:_files -/' \
            '--main=[Main branch]:branch:' \
            '--install=[Install deps]:mode:(auto true false)' \
            '--supabase=[Supabase]:mode:(auto true false)' \
            '--supabase-config=[Repo-relative Supabase config path]:file:_files' \
            '--db=[DB setup]:mode:(auto true false)'
          return
          ;;
        init)
          _arguments -C \
            '--force[Overwrite existing .wrt.json]' \
            '--print[Print config and exit]' \
            '--accept-commands[Accept generated executable commands]' \
            '--model=[Codex model]:model:'
          return
          ;;
        root)
          case $line[2] in
            init)
              _arguments -C \
                '1:source:_files' \
                '--root=[Managed root directory]:directory:_files -/' \
                '--main=[Main branch]:branch:' \
                '--install=[Install deps]:mode:(auto true false)' \
                '--supabase=[Supabase]:mode:(auto true false)' \
                '--supabase-config=[Repo-relative Supabase config path]:file:_files' \
                '--db=[DB setup]:mode:(auto true false)'
              return
              ;;
            status)
              return
              ;;
            *)
              _values 'root command' \
                'init[Create a managed root]' \
                'status[Print managed-root status]'
              return
              ;;
          esac
          ;;
        housekeeping)
          _arguments '--apply[Delete candidates]'
          return
          ;;
        completions)
          _arguments '1:shell:(zsh)'
          return
          ;;
      esac
      ;;
  esac
}

_wrt "$@"
