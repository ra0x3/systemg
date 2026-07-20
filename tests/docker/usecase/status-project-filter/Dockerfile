FROM sysg-usecase-base

COPY tests/docker/usecase/status-project-filter/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/status-project-filter/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
