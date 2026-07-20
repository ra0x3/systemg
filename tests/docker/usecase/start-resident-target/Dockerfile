FROM sysg-usecase-base

COPY tests/docker/usecase/start-resident-target/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/start-resident-target/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
